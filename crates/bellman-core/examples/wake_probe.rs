//! Print the live wake capability status line (P7 probe).
fn main() {
    let w = bellman_core::create_wake();
    let cap = w.capability();
    println!("{}", cap.status_line());
    match &cap {
        bellman_core::WakeCapability::Enabled { mechanism, caveats } => {
            println!("mechanism={mechanism:?}");
            println!("caveats={caveats:?}");
        }
        bellman_core::WakeCapability::Disabled { reason } => {
            println!("reason={reason:?}");
            if let Some(h) = reason.fix_hint() {
                println!("fix_hint={h}");
            }
        }
    }
}
