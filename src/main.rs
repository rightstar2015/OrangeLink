#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod clipboard;
mod config;
#[cfg(test)]
mod fuzz;
mod export;
mod grouping;
mod hotkey;
mod self_update;
mod icon;
mod imaging;
mod importer;
mod library;
mod loader;
mod log;
mod mcp;
mod media;
#[cfg(windows)]
mod mf_player;
mod model;
mod pdf;
mod player;
mod premiere;
mod process;
mod release_keys;
// The signing tool's latest.json reader, so the app's tests can hold it to
// serde_json (update.rs). Not part of the app.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../tools/sign_release/src/latest.rs"]
mod sign_release_latest;
#[cfg(test)]
mod release_workflow;
mod signals;
mod similarity;
mod shell_menu;
mod shell_thumb;
mod ui;
mod update;
mod watch;
mod webclip;
mod whisper;
mod ytdlp;

use eframe::egui;

/// A lock that still works after another thread panicked while holding it:
/// what it guards is plain state that stays usable, and one failed thread
/// shouldn't bring down every other one that touches it.
#[allow(clippy::disallowed_methods)] // the one place a Mutex is locked
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Waits up to `limit` for `thread` to end, and joins it once it has: the join
/// returns only when the operating system thread is gone, thread-local and COM
/// clean-up included. A thread that runs past `limit` is left to finish alone, so
/// a stuck one can't hold the caller (the UI thread) for good.
pub(crate) fn join_within(thread: std::thread::JoinHandle<()>, limit: std::time::Duration) {
    let end = std::time::Instant::now() + limit;
    while !thread.is_finished() {
        if std::time::Instant::now() >= end {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let _ = thread.join();
}

fn window_icon() -> egui::IconData {
    let img = icon::render(128);
    egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() }
}

fn main() -> eframe::Result<()> {
    // Launched by an AI client as a stdio MCP server: bridge to the running window.
    if std::env::args().any(|a| a == "--mcp-stdio") {
        let config = config::Config::load();
        mcp::run_stdio_proxy(config.mcp_port, &config.mcp_token);
        return Ok(());
    }

    // Started by the old version on exit: take its place (see self_update.rs).
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--apply-update") {
        let relaunch = args.iter().any(|a| a == "--relaunch");
        if let Some(target) = args.get(i + 1) {
            match self_update::apply(std::path::Path::new(target), relaunch) {
                Ok(()) => log::info("update", format!("已更新到 {}", env!("CARGO_PKG_VERSION"))),
                Err(e) => log::error("update", format!("安裝更新失敗：{e}")),
            }
        }
        return Ok(());
    }
    self_update::clean_up();

    log::install_panic_hook();
    log::info("start", format!("OrangeLink {} 啟動", env!("CARGO_PKG_VERSION")));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("OrangeLink")
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([960.0, 600.0])
        .with_drag_and_drop(true)
        .with_icon(window_icon());
    // Reopen where the window was last closed.
    if let Some(g) = config::Config::load().window.filter(|g| g.on_screen()) {
        viewport = viewport
            .with_position([g.x, g.y])
            .with_inner_size([g.w.max(960.0), g.h.max(600.0)])
            .with_maximized(g.maximized);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "OrangeLink",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
