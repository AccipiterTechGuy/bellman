//! Bellman macOS wake daemon — own-tag IOPMSchedulePowerEvent one-shots.
//!
//! Protocol (newline-delimited JSON over `/var/run/bellman-wake.sock`):
//!   {"op":"schedule","epoch":N,"tag":"com.bellman.wake"}
//!   {"op":"cancel_my_wakes","tag":"com.bellman.wake"}
//! Responses: {"ok":true} | {"ok":false,"error":"..."}
//!
//! Full IOKit + SMAppService + code-sig validation land in the macOS package
//! build. This binary is a protocol-compatible scaffold that refuses to run
//! as non-root and documents the surface.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

const SOCK: &str = "/var/run/bellman-wake.sock";
const OWN_TAG: &str = "com.bellman.wake";

fn main() {
    if !cfg!(target_os = "macos") {
        eprintln!("bellman-wake-daemon: only meaningful on macOS");
        std::process::exit(1);
    }
    // Root required for IOPMSchedulePowerEvent.
    #[cfg(unix)]
    {
        if unsafe { libc_geteuid() } != 0 {
            eprintln!("bellman-wake-daemon: must run as root (SMAppService)");
            std::process::exit(1);
        }
    }
    let _ = std::fs::remove_file(SOCK);
    let listener = UnixListener::bind(SOCK).expect("bind wake socket");
    // Restrict to root + bellman group when packaging sets ownership.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(SOCK, std::fs::Permissions::from_mode(0o660));
    }
    eprintln!("bellman-wake-daemon: listening on {SOCK}");
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                if let Err(e) = handle(stream) {
                    eprintln!("bellman-wake-daemon: {e}");
                }
            }
            Err(e) => eprintln!("bellman-wake-daemon: accept: {e}"),
        }
    }
}

fn handle(stream: UnixStream) -> Result<(), String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let v: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|e| format!("bad json: {e}"))?;
    let op = v.get("op").and_then(|x| x.as_str()).unwrap_or("");
    let tag = v
        .get("tag")
        .and_then(|x| x.as_str())
        .unwrap_or(OWN_TAG);
    if tag != OWN_TAG {
        return reply(&stream, false, "foreign tag refused");
    }
    match op {
        "schedule" => {
            let epoch = v
                .get("epoch")
                .and_then(|x| x.as_i64())
                .ok_or_else(|| "schedule requires epoch".to_string())?;
            // Real build: IOPMSchedulePowerEvent(kIOPMAutoWake, OWN_TAG).
            // Scaffold acknowledges after validating the request shape.
            let _ = epoch;
            reply(&stream, true, "")
        }
        "cancel_my_wakes" => {
            // Real build: cancel by exact (time, id, type) match only.
            reply(&stream, true, "")
        }
        _ => reply(&stream, false, "unknown op"),
    }
}

fn reply(mut stream: &UnixStream, ok: bool, err: &str) -> Result<(), String> {
    let body = if ok {
        r#"{"ok":true}"#.to_string()
    } else {
        format!(r#"{{"ok":false,"error":{}}}"#, serde_json::to_string(err).unwrap())
    };
    writeln!(stream, "{body}").map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(unix)]
fn libc_geteuid() -> u32 {
    // Avoid a libc crate dep in the scaffold: use the libc syscall via extern.
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

#[allow(dead_code)]
fn _path_exists() -> bool {
    Path::new(SOCK).exists()
}
