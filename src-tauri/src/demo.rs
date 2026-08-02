//! WIZ1 — the lightbulb demo: path resolution, interpreter probe, launching.
//!
//! Bellman's job here is **explaining and launching**, never provisioning:
//! the demo app creates its own timer through the slot protocol
//! (`docs/INTEGRATION.md`, DEMO1). This module only knows how to:
//!
//! - find the demo files (installed package location first, then the source
//!   tree when running from a dev checkout),
//! - probe whether `python3` + `tkinter` can actually run the GUI demo
//!   (on Ubuntu 24.04 tkinter is the separate `python3-tk` package — a
//!   `python3`-only check would offer a button that fails on a stock
//!   desktop),
//! - launch the demo as an ordinary detached child process pointed at this
//!   install's slots root. Bellman does not supervise it; closing Bellman
//!   does not kill it.
//!
//! `BELLMAN_DEMO_DIR` overrides the search (used by deb-payload verification
//! and by packagers whose layout differs); it is never set in production.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Where the Linux package installs the demos (`bundle.linux.deb.files`).
pub const INSTALLED_DEMO_ROOT: &str = "/usr/share/bellman/testing_apps";
/// Where the Linux package installs the integration doc the panel links to.
pub const INSTALLED_INTEGRATION_DOC: &str = "/usr/share/bellman/docs/INTEGRATION.md";

/// The GUI demo's entry point, relative to a demo root.
const DEMO_PY_REL: &str = "lightbulb_gui/lightbulb_gui.py";

/// Snapshot the wizard panel / Settings render from. camelCase at the IPC
/// boundary. `command` is the copyable run command; `canLaunch` gates the
/// **Run the demo** button (never show a button that cannot work).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DemoInfoDto {
    /// The persisted wizard choice (`demo_opt_in`) — Settings mirrors it.
    pub opt_in: bool,
    /// True when a demo directory was actually resolved on this machine.
    pub available: bool,
    /// The resolved demo directory (absent when unavailable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demo_dir: Option<String>,
    /// The copyable run command (absent when unavailable — never fabricated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// available AND python3 present AND tkinter importable.
    pub can_launch: bool,
    /// Whether `python3` is on `PATH`.
    pub python3_present: bool,
    /// Whether that `python3` can `import tkinter`. Without it the GUI demo
    /// cannot run, so the Run button is hidden and the python3-tk note is
    /// shown instead of offering a button that would fail.
    pub tkinter_present: bool,
    /// This install's slots root — the demo must be pointed at it.
    pub slots_dir: String,
    /// Resolved `docs/INTEGRATION.md` for "connect your own application"
    /// (absent on platforms whose packaging does not carry it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_path: Option<String>,
}

/// The demo root candidates, in priority order: env override (tests /
/// packagers), the installed package location, then the dev source tree.
fn demo_root_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::var_os("BELLMAN_DEMO_DIR") {
        if !dir.is_empty() {
            out.push(PathBuf::from(dir));
        }
    }
    out.push(PathBuf::from(INSTALLED_DEMO_ROOT));
    // Dev checkout: CARGO_MANIFEST_DIR is `<repo>/src-tauri` at build time.
    if let Some(repo) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        out.push(repo.join("testing_apps"));
    }
    out
}

/// First candidate that actually contains the GUI demo entry point.
fn resolve_demo_dir_in(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|dir| dir.join(DEMO_PY_REL).is_file())
        .cloned()
}

/// Resolve the demo directory for this install (installed location first,
/// then the source tree). `None` when the platform packaging does not carry
/// the files — callers degrade honestly and never fabricate a path.
pub fn resolve_demo_dir() -> Option<PathBuf> {
    resolve_demo_dir_in(&demo_root_candidates())
}

/// Resolve `docs/INTEGRATION.md` (installed location first, then the source
/// tree). `None` when neither exists.
pub fn resolve_integration_doc() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from(INSTALLED_INTEGRATION_DOC)];
    if let Some(repo) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
        candidates.push(repo.join("docs").join("INTEGRATION.md"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Probe `python3` and, more importantly, tkinter. Distinct outcomes:
/// - interpreter missing entirely → `(false, false)`
/// - interpreter present, `import tkinter` fails (stock Ubuntu 24.04
///   without `python3-tk`) → `(true, false)`
/// - both present → `(true, true)`
pub fn probe_python_tkinter(python: &Path) -> (bool, bool) {
    match std::process::Command::new(python)
        .arg("-c")
        .arg("import tkinter")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) => (true, status.success()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (false, false),
        // Spawn failed for another reason (permissions, …): the interpreter
        // exists on PATH but cannot run the demo — treat as unusable.
        Err(_) => (true, false),
    }
}

/// Shell-quote one argument for the copyable command (single-quote style).
fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The exact command the panel shows in its copyable field.
pub fn display_command(demo_dir: &Path, slots_dir: &Path) -> String {
    let demo_py = demo_dir.join(DEMO_PY_REL);
    format!(
        "python3 {} --slots {}",
        sh_quote(&demo_py.display().to_string()),
        sh_quote(&slots_dir.display().to_string())
    )
}

/// Gather the full snapshot for the wizard panel / Settings.
pub fn gather(opt_in: bool, slots_dir: &Path) -> DemoInfoDto {
    let demo_dir = resolve_demo_dir();
    let (python3_present, tkinter_present) = probe_python_tkinter(Path::new("python3"));
    let can_launch = demo_dir.is_some() && python3_present && tkinter_present;
    DemoInfoDto {
        opt_in,
        available: demo_dir.is_some(),
        command: demo_dir.as_ref().map(|dir| display_command(dir, slots_dir)),
        demo_dir: demo_dir.map(|dir| dir.display().to_string()),
        can_launch,
        python3_present,
        tkinter_present,
        slots_dir: slots_dir.display().to_string(),
        docs_path: resolve_integration_doc().map(|p| p.display().to_string()),
    }
}

/// Launch the demo as an ordinary detached child process.
///
/// Re-probes the interpreter first: launching a GUI that cannot start would
/// fail silently, so the button path and the command agree that a demo
/// without tkinter is not launchable ("never a button that cannot work").
///
/// The child gets null stdio and (on Unix) its own process group, so
/// closing or killing Bellman leaves the demo running — it is a separate
/// application and behaves like one.
pub fn launch_demo(slots_dir: &Path) -> Result<u32, String> {
    let demo_dir =
        resolve_demo_dir().ok_or_else(|| "demo files not found on this machine".to_string())?;
    let demo_py = demo_dir.join(DEMO_PY_REL);
    let (python3_present, tkinter_present) = probe_python_tkinter(Path::new("python3"));
    if !python3_present {
        return Err("python3 is not installed".to_string());
    }
    if !tkinter_present {
        return Err("python3-tk (tkinter) is not installed".to_string());
    }

    let mut cmd = std::process::Command::new("python3");
    cmd.arg(&demo_py)
        .arg("--slots")
        .arg(slots_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Own process group: not signalled with Bellman's group, survives us.
        cmd.process_group(0);
    }
    let child = cmd.spawn().map_err(|e| format!("spawn demo: {e}"))?;
    Ok(child.id())
}

/// Open the integration doc with the platform's default handler, detached.
pub fn open_integration_doc() -> Result<(), String> {
    let doc = resolve_integration_doc()
        .ok_or_else(|| "docs/INTEGRATION.md not found on this machine".to_string())?;
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.arg("/c").arg("start").arg("").arg(&doc);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(&doc);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&doc);
        c
    };
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().map_err(|e| format!("open doc: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `python3` shell script whose `import tkinter` probe succeeds
    /// or fails as ordered. Used to pin the Ubuntu-24.04-without-python3-tk
    /// case, which a `python3`-only existence check passes wrongly.
    fn write_fake_python3(dir: &Path, tkinter_ok: bool) -> PathBuf {
        let script = dir.join("python3");
        let body = if tkinter_ok {
            "#!/bin/sh\nexit 0\n"
        } else {
            "#!/bin/sh\necho \"ModuleNotFoundError: No module named 'tkinter'\" >&2\nexit 1\n"
        };
        std::fs::write(&script, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        script
    }

    fn write_demo(root: &Path) {
        let dir = root.join("lightbulb_gui");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lightbulb_gui.py"), "# demo\n").unwrap();
    }

    #[test]
    fn resolve_prefers_first_candidate_that_has_the_demo() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("installed");
        let b = tmp.path().join("dev");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        // Neither has the demo yet.
        assert_eq!(resolve_demo_dir_in(&[a.clone(), b.clone()]), None);
        // Only the dev tree has it → dev wins.
        write_demo(&b);
        assert_eq!(
            resolve_demo_dir_in(&[a.clone(), b.clone()]),
            Some(b.clone())
        );
        // Installed location appears → it wins over the source tree.
        write_demo(&a);
        assert_eq!(resolve_demo_dir_in(&[a.clone(), b.clone()]), Some(a));
    }

    #[test]
    fn probe_distinguishes_missing_tkinter_from_missing_python() {
        let tmp = tempfile::tempdir().unwrap();
        let with_tk = write_fake_python3(tmp.path(), true);
        assert_eq!(
            probe_python_tkinter(&with_tk),
            (true, true),
            "python3 + tkinter present"
        );
        let without_tk = write_fake_python3(tmp.path(), false);
        assert_eq!(
            probe_python_tkinter(&without_tk),
            (true, false),
            "python3 present but tkinter missing (stock Ubuntu 24.04 w/o python3-tk)"
        );
        let missing = tmp.path().join("no-such-python3");
        assert_eq!(probe_python_tkinter(&missing), (false, false));
    }

    #[test]
    fn display_command_points_at_slots_root_and_quotes_spaces() {
        let demo = Path::new("/usr/share/bellman/testing_apps");
        let slots = Path::new("/home/u/.local/share/io.bellman.desktop/slots");
        assert_eq!(
            display_command(demo, slots),
            "python3 /usr/share/bellman/testing_apps/lightbulb_gui/lightbulb_gui.py --slots /home/u/.local/share/io.bellman.desktop/slots"
        );
        let spaced = Path::new("/opt/my apps/testing_apps");
        assert_eq!(
            display_command(spaced, slots),
            "python3 '/opt/my apps/testing_apps/lightbulb_gui/lightbulb_gui.py' --slots /home/u/.local/share/io.bellman.desktop/slots"
        );
    }
}
