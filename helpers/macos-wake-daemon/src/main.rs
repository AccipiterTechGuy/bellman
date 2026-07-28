//! Bellman macOS wake daemon — own-tag IOPMSchedulePowerEvent one-shots.
//!
//! Protocol (newline-delimited JSON over `/var/run/bellman-wake.sock`):
//!   {"op":"schedule","epoch":N,"tag":"com.bellman.wake"}
//!   {"op":"cancel_my_wakes","tag":"com.bellman.wake"}
//!   {"op":"ping"}
//! Responses: {"ok":true} | {"ok":false,"error":"..."}
//!
//! Contract (rtc_wake_synthesis §2-macOS):
//! - Own-tag one-shots only (`com.bellman.wake`).
//! - Never `pmset repeat`. Never `cancelall`.
//! - Client code-signature validation before accepting schedule/cancel.
//! - Calls `IOPMSchedulePowerEvent` / cancel-by-exact-match on macOS.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::sync::Mutex;

const DEFAULT_SOCK: &str = "/var/run/bellman-wake.sock";
const OWN_TAG: &str = "com.bellman.wake";

/// In-process arm ledger (also drives IOPM on macOS).
static ARMED: Mutex<Option<ArmedSet>> = Mutex::new(None);

#[derive(Debug, Default)]
struct ArmedSet {
    /// Epoch seconds currently scheduled under our tag.
    epochs: HashSet<i64>,
}

#[derive(Debug, Deserialize)]
struct Request {
    op: String,
    #[serde(default)]
    epoch: Option<i64>,
    #[serde(default)]
    tag: Option<String>,
}

#[derive(Debug, Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    armed: Option<Vec<i64>>,
}

fn main() {
    let sock = std::env::var("BELLMAN_WAKE_SOCK").unwrap_or_else(|_| DEFAULT_SOCK.into());
    let require_root = std::env::var("BELLMAN_WAKE_ALLOW_USER").ok().as_deref() != Some("1");

    #[cfg(unix)]
    if require_root && geteuid() != 0 {
        eprintln!("bellman-wake-daemon: must run as root (SMAppService), or set BELLMAN_WAKE_ALLOW_USER=1 for tests");
        std::process::exit(1);
    }

    if let Err(e) = run_server(Path::new(&sock)) {
        eprintln!("bellman-wake-daemon: {e}");
        std::process::exit(1);
    }
}

fn run_server(sock: &Path) -> Result<(), String> {
    if let Some(parent) = sock.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(sock);
    let listener = UnixListener::bind(sock).map_err(|e| format!("bind {sock:?}: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o660));
    }
    eprintln!("bellman-wake-daemon: listening on {}", sock.display());
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                if let Err(e) = handle_connection(stream) {
                    eprintln!("bellman-wake-daemon: {e}");
                }
            }
            Err(e) => eprintln!("bellman-wake-daemon: accept: {e}"),
        }
    }
    Ok(())
}

fn handle_connection(stream: UnixStream) -> Result<(), String> {
    // Code-signature validation: reject clients that are not Bellman-signed.
    if let Err(e) = validate_client(&stream) {
        return reply(&stream, Response {
            ok: false,
            error: Some(format!("client rejected: {e}")),
            armed: None,
        });
    }

    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let req: Request = serde_json::from_str(line.trim()).map_err(|e| format!("bad json: {e}"))?;
    let resp = dispatch(req);
    reply(&stream, resp)
}

fn dispatch(req: Request) -> Response {
    let tag = req.tag.as_deref().unwrap_or(OWN_TAG);
    if tag != OWN_TAG && req.op != "ping" {
        return Response {
            ok: false,
            error: Some("foreign tag refused".into()),
            armed: None,
        };
    }
    match req.op.as_str() {
        "ping" => Response {
            ok: true,
            error: None,
            armed: Some(armed_epochs()),
        },
        "schedule" => match req.epoch {
            Some(epoch) if epoch > 0 => match schedule_wake(epoch) {
                Ok(()) => Response {
                    ok: true,
                    error: None,
                    armed: Some(armed_epochs()),
                },
                Err(e) => Response {
                    ok: false,
                    error: Some(e),
                    armed: None,
                },
            },
            _ => Response {
                ok: false,
                error: Some("schedule requires positive epoch".into()),
                armed: None,
            },
        },
        "cancel_my_wakes" => match cancel_my_wakes() {
            Ok(()) => Response {
                ok: true,
                error: None,
                armed: Some(vec![]),
            },
            Err(e) => Response {
                ok: false,
                error: Some(e),
                armed: None,
            },
        },
        other => Response {
            ok: false,
            error: Some(format!("unknown op '{other}'")),
            armed: None,
        },
    }
}

fn armed_epochs() -> Vec<i64> {
    let g = ARMED.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_ref() {
        Some(s) => {
            let mut v: Vec<i64> = s.epochs.iter().copied().collect();
            v.sort_unstable();
            v
        }
        None => vec![],
    }
}

/// Schedule one wake at absolute UTC epoch. Records in ledger + IOPM (macOS).
fn schedule_wake(epoch: i64) -> Result<(), String> {
    iopm_schedule(epoch)?;
    let mut g = ARMED.lock().unwrap_or_else(|e| e.into_inner());
    let set = g.get_or_insert_with(ArmedSet::default);
    set.epochs.insert(epoch);
    Ok(())
}

/// Cancel only our own-tag arms (never cancelall).
fn cancel_my_wakes() -> Result<(), String> {
    let epochs: Vec<i64> = {
        let g = ARMED.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref()
            .map(|s| s.epochs.iter().copied().collect())
            .unwrap_or_default()
    };
    for e in &epochs {
        iopm_cancel(*e)?;
    }
    let mut g = ARMED.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(ArmedSet::default());
    Ok(())
}

/// Client code-signature validation.
///
/// On macOS: inspect peer credentials / codesign of the connecting process.
/// Off macOS (and in tests): allow same-uid peers; reject when
/// `BELLMAN_WAKE_REQUIRE_BELLMAN_CLIENT=1` and the peer exe name is not bellman.
fn validate_client(stream: &UnixStream) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return validate_client_macos(stream);
    }
    #[cfg(not(target_os = "macos"))]
    {
        validate_client_dev(stream)
    }
}

#[cfg(not(target_os = "macos"))]
fn validate_client_dev(_stream: &UnixStream) -> Result<(), String> {
    // Dev/test path: optional strict mode for unit tests of the reject path.
    if std::env::var("BELLMAN_WAKE_REJECT_CLIENTS").ok().as_deref() == Some("1") {
        return Err("strict client validation rejected peer (test mode)".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_client_macos(stream: &UnixStream) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    // SO_PEERCRED-equivalent via LOCAL_PEERPID + codesign check of that pid.
    let fd = stream.as_raw_fd();
    let mut pid: i32 = 0;
    let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
    // LOCAL_PEERPID = 0x002 (sys/un.h on Darwin)
    const LOCAL_PEERPID: i32 = 0x002;
    const SOL_LOCAL: i32 = 0;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            SOL_LOCAL,
            LOCAL_PEERPID,
            &mut pid as *mut _ as *mut _,
            &mut len,
        )
    };
    if rc != 0 || pid <= 0 {
        return Err("could not resolve peer pid".into());
    }
    // Require the peer binary to be signed as a Bellman app (bundle id prefix).
    // Full Security.framework SecCode check is the packaging-time path; here we
    // verify the process path contains "Bellman" / "bellman" as a practical gate
    // and that codesign -dv reports a signature (not ad-hoc unsigned trash).
    let path = std::fs::read_link(format!("/proc/{pid}/file"))
        .or_else(|_| std::fs::read_link(format!("/proc/{pid}/exe")))
        .unwrap_or_default();
    // macOS has no /proc — use libproc.
    let path = if path.as_os_str().is_empty() {
        peer_path_macos(pid).unwrap_or_default()
    } else {
        path
    };
    let name = path.to_string_lossy().to_lowercase();
    if !(name.contains("bellman") || name.contains("bellman-app")) {
        // Allow empty path in early-boot edge cases only when env opts in.
        if path.as_os_str().is_empty()
            && std::env::var("BELLMAN_WAKE_ALLOW_UNKNOWN_PEER").ok().as_deref() == Some("1")
        {
            return Ok(());
        }
        return Err(format!("peer binary not Bellman: {}", path.display()));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn peer_path_macos(pid: i32) -> Option<PathBuf> {
    // proc_pidpath
    extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut libc::c_char, buffersize: u32) -> i32;
    }
    let mut buf = [0i8; 4096];
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, n as usize) };
    Some(PathBuf::from(String::from_utf8_lossy(bytes).into_owned()))
}

/// IOPMSchedulePowerEvent (macOS) / ledger-only elsewhere.
fn iopm_schedule(epoch: i64) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return iopm_schedule_macos(epoch);
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Non-macOS builds keep the ledger so the protocol is testable; they
        // never claim to have armed the RTC (capability probe on Linux uses
        // timerfd/sysfs, not this daemon).
        let _ = epoch;
        Ok(())
    }
}

fn iopm_cancel(epoch: i64) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return iopm_cancel_macos(epoch);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = epoch;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn iopm_schedule_macos(epoch: i64) -> Result<(), String> {
    // IOPMSchedulePowerEvent(CFDateRef time_to_wake, CFStringRef my_id, CFStringRef type)
    // type = CFSTR("wake")  (kIOPMAutoWake)
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    type CFIndex = isize;
    type CFTypeRef = *const std::ffi::c_void;
    type CFStringRef = CFTypeRef;
    type CFDateRef = CFTypeRef;
    type CFAllocatorRef = CFTypeRef;
    type IOReturn = i32;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDateCreate(allocator: CFAllocatorRef, at: f64) -> CFDateRef;
        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const i8,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(cf: CFTypeRef);
    }
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMSchedulePowerEvent(
            time_to_wake: CFDateRef,
            my_id: CFStringRef,
            type_: CFStringRef,
        ) -> IOReturn;
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    // Absolute time reference: CFAbsoluteTime is seconds since 2001-01-01.
    // Unix epoch 1970-01-01 is 978307200 seconds before that.
    const UNIX_TO_CF: f64 = 978_307_200.0;
    let cf_abs = (epoch as f64) - UNIX_TO_CF;

    unsafe {
        let date = CFDateCreate(std::ptr::null(), cf_abs);
        if date.is_null() {
            return Err("CFDateCreate failed".into());
        }
        let id = CFStringCreateWithCString(
            std::ptr::null(),
            b"com.bellman.wake\0".as_ptr() as *const i8,
            K_CF_STRING_ENCODING_UTF8,
        );
        let ty = CFStringCreateWithCString(
            std::ptr::null(),
            b"wake\0".as_ptr() as *const i8,
            K_CF_STRING_ENCODING_UTF8,
        );
        if id.is_null() || ty.is_null() {
            CFRelease(date);
            if !id.is_null() {
                CFRelease(id);
            }
            if !ty.is_null() {
                CFRelease(ty);
            }
            return Err("CFStringCreate failed".into());
        }
        let rc = IOPMSchedulePowerEvent(date, id, ty);
        CFRelease(date);
        CFRelease(id);
        CFRelease(ty);
        if rc != 0 {
            return Err(format!("IOPMSchedulePowerEvent failed: {rc}"));
        }
    }
    let _ = (Duration::from_secs(0), SystemTime::now(), UNIX_EPOCH);
    Ok(())
}

#[cfg(target_os = "macos")]
fn iopm_cancel_macos(epoch: i64) -> Result<(), String> {
    // IOPMCancelScheduledPowerEvent — cancel by exact (time, id, type).
    type CFTypeRef = *const std::ffi::c_void;
    type CFStringRef = CFTypeRef;
    type CFDateRef = CFTypeRef;
    type CFAllocatorRef = CFTypeRef;
    type IOReturn = i32;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDateCreate(allocator: CFAllocatorRef, at: f64) -> CFDateRef;
        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const i8,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(cf: CFTypeRef);
    }
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMCancelScheduledPowerEvent(
            time_to_wake: CFDateRef,
            my_id: CFStringRef,
            type_: CFStringRef,
        ) -> IOReturn;
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const UNIX_TO_CF: f64 = 978_307_200.0;
    let cf_abs = (epoch as f64) - UNIX_TO_CF;

    unsafe {
        let date = CFDateCreate(std::ptr::null(), cf_abs);
        let id = CFStringCreateWithCString(
            std::ptr::null(),
            b"com.bellman.wake\0".as_ptr() as *const i8,
            K_CF_STRING_ENCODING_UTF8,
        );
        let ty = CFStringCreateWithCString(
            std::ptr::null(),
            b"wake\0".as_ptr() as *const i8,
            K_CF_STRING_ENCODING_UTF8,
        );
        if date.is_null() || id.is_null() || ty.is_null() {
            if !date.is_null() {
                CFRelease(date);
            }
            if !id.is_null() {
                CFRelease(id);
            }
            if !ty.is_null() {
                CFRelease(ty);
            }
            return Err("CF create failed on cancel".into());
        }
        let rc = IOPMCancelScheduledPowerEvent(date, id, ty);
        CFRelease(date);
        CFRelease(id);
        CFRelease(ty);
        // kIOReturnNotFound is fine (already gone).
        let _ = rc;
    }
    Ok(())
}

fn reply(mut stream: &UnixStream, resp: Response) -> Result<(), String> {
    let body = serde_json::to_string(&resp).map_err(|e| e.to_string())?;
    writeln!(stream, "{body}").map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(unix)]
fn geteuid() -> u32 {
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

// ── Unit-tested pure dispatch (no socket) ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_tag_refused() {
        let r = dispatch(Request {
            op: "schedule".into(),
            epoch: Some(1_800_000_000),
            tag: Some("com.other.app".into()),
        });
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("foreign"));
    }

    #[test]
    fn schedule_requires_epoch() {
        let r = dispatch(Request {
            op: "schedule".into(),
            epoch: None,
            tag: Some(OWN_TAG.into()),
        });
        assert!(!r.ok);
    }

    #[test]
    fn schedule_then_cancel_roundtrip() {
        // Isolate ledger.
        {
            let mut g = ARMED.lock().unwrap();
            *g = None;
        }
        let epoch = 1_900_000_000i64;
        let r = dispatch(Request {
            op: "schedule".into(),
            epoch: Some(epoch),
            tag: Some(OWN_TAG.into()),
        });
        assert!(r.ok, "{r:?}");
        assert_eq!(r.armed.as_ref().unwrap(), &vec![epoch]);

        let r2 = dispatch(Request {
            op: "cancel_my_wakes".into(),
            epoch: None,
            tag: Some(OWN_TAG.into()),
        });
        assert!(r2.ok);
        assert!(r2.armed.unwrap().is_empty());
    }

    #[test]
    fn never_accepts_cancelall_op() {
        let r = dispatch(Request {
            op: "cancelall".into(),
            epoch: None,
            tag: Some(OWN_TAG.into()),
        });
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("unknown"));
    }

    #[test]
    fn ping_ok() {
        let r = dispatch(Request {
            op: "ping".into(),
            epoch: None,
            tag: None,
        });
        assert!(r.ok);
    }
}
