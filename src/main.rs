#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! Internet Control — usage meter v1.0.
//!
//! Windows 10/11 x64. Counts real download/upload by capturing packets
//! with WinDivert — the standard adapter counters never move on PDAnet
//! PPP adapters, so packet counting is the only way.
//!
//! v1.0 rewrite:
//! - The meter only SNIFFS (packets are copies, never held): counting
//!   adds zero latency and can never stall the internet. v0.3–v0.9
//!   routed every packet of the system through one user-space thread.
//! - Blocking is its own engine (`blocker.rs`): WFP dynamic-session
//!   filters per app + a kernel-side WinDivert DROP handle for sockets
//!   that were already open + immediate TCP termination. Everything
//!   disappears when the app exits or crashes.
//! - New native UI in Slint (`ui/app.slint`, `ui.rs`), software
//!   rendered: no GPU, no WebView2/Edge. Borderless, dark/light,
//!   Minimal mini-meter with graph, pin on top, opacity, search.
//! - Adapter enumeration every 2 s instead of 10x a second.
//! - The exe embeds a requireAdministrator manifest even when
//!   cross-compiled.
//! Falls back to the adapter counters if WinDivert cannot be opened.

mod adapters;
mod blocker;
mod capture;
mod connections;
mod log;
mod meter;
mod opacity;
mod public_ip;
mod startup;
mod state;
mod store;
mod ui;
mod unknown;
mod win32;

use std::sync::mpsc;
use std::sync::Arc;

fn main() {
    log::init();
    if let Some(p) = log::path() {
        log::log(&format!("log file: {}", p.display()));
    }
    log::log(&format!("Internet Control v{} started", env!("CARGO_PKG_VERSION")));
    let store = store::Store::load();
    let shared = Arc::new(state::Shared::new(store));

    // Sampler thread: WinDivert capture + 100 ms batches.
    let (tx, rx) = mpsc::channel();
    let blocked_for_sampler = shared.blocked.clone();
    std::thread::spawn(move || adapters::sample_loop(tx, blocked_for_sampler));

    // Bridge thread: batches -> the snapshot the UI reads.
    let bridge_shared = shared.clone();
    std::thread::spawn(move || {
        let mut bridge = state::Bridge::new(bridge_shared);
        while let Ok(batch) = rx.recv() {
            bridge.apply(batch);
        }
    });

    // Public IP + country lookup thread (v0.9).
    let ip_shared = shared.clone();
    std::thread::spawn(move || crate::public_ip::run_loop(ip_shared));

    if let Err(e) = ui::run(shared.clone()) {
        log::log(&format!("FATAL: could not open the window: {e}"));
        eprintln!("Could not open the window: {e}");
        std::process::exit(1);
    }

    log::log("window closed, saving config and exiting");
    shared
        .store
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .save();
}
