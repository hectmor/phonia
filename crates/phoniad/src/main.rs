//! `phoniad` -- skeleton for the phonia daemon.
//!
//! Not implemented yet: the playback engine, the queue and the IPC protocol between this daemon
//! and the future TUI client are tracked in issues #10-#12. This binary intentionally does
//! nothing else than say so.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("phoniad: the daemon is not implemented yet.");
    println!("See issues #10 (playback engine), #11 (queue) and #12 (IPC) for what's next.");
    Ok(())
}
