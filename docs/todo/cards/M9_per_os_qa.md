# M9 — Per-OS QA on real hardware + the ship guard

Design: whole of `macro_recorder_security_plan.md`; synthesis §5 C-M8.

## Scope

Real-hardware validation — not mocks — of capture, injection, the panic key, keyring
semantics, macOS TCC, and Windows UIPI on:

- Windows 11
- macOS 13+
- Ubuntu GNOME/Wayland — **authoring only**; replay is expected to report `Unavailable` (D-16)
- KDE / X11

Plus the destructive matrix in disposable VMs: screen lock, suspend/resume, reboot, changed
DPI, changed monitor layout, an elevated Windows target, denied macOS permissions, and
X11-vs-Wayland detection.

And: the artifact marker-grep wired into the release pipeline, plus docs.

## Exit gate

- Fresh-VM install per OS: author → review → save → dry-run → attach → fires overnight →
  **the audit log reconstructs the whole thing.**
- The marker grep is proven to fail a **deliberately poisoned** build — a guard nobody has
  seen fail is not a guard.
- Windows UIPI: injection into an elevated window is detected and reported, not silently
  dropped.
- macOS: the Accessibility grant survives an app update, or the docs say plainly that it
  does not and why (signing identity).
