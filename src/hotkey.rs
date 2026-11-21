//! A system-wide shortcut (default Ctrl+Alt+Shift+O; Ctrl+Alt+Space is often taken) that brings OrangeLink to the front
//! with the search box focused, from Premiere or anywhere else.
//!
//! Registration follows Prime Files (`crates/nohrs-launcher/src/hotkey.rs`, the
//! `global-hotkey` crate), MIT License, Copyright (c) 2025 Nohrs contributors,
//! (c) 2026 Prime Files contributors.

use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::{
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

pub const DEFAULT: &str = "Ctrl+Alt+Shift+O";

/// "Ctrl+Alt+Space", "Ctrl+Shift+F" and the like, in the crate's terms: modifiers are
/// Ctrl/Alt/Shift/Win, keys are letters, digits, F1..F24, Space and so on.
pub fn parse(spec: &str) -> Result<HotKey, String> {
    let spec = spec.trim().replace("Win+", "Super+").replace("win+", "super+");
    let key = HotKey::from_str(&spec).map_err(|e| t!("看不懂快捷鍵「{spec}」：{e}", spec, e))?;
    if key.mods.is_empty() {
        return Err(t!("快捷鍵要包含 Ctrl、Alt、Shift 或 Win，否則一般打字也會觸發").into());
    }
    Ok(key)
}

/// The registered shortcut; dropping it unregisters.
pub struct Hotkey {
    manager: GlobalHotKeyManager,
    key: HotKey,
    fired: Arc<AtomicBool>,
}

impl Hotkey {
    /// Must run on the UI thread: on Windows the shortcut arrives as a window message
    /// on the thread that registered it, which eframe's event loop dispatches.
    /// `wake` runs when it is pressed, even while the window is minimized.
    pub fn register(spec: &str, wake: impl Fn() + Send + Sync + 'static) -> Result<Self, String> {
        let key = parse(spec)?;
        let manager = GlobalHotKeyManager::new().map_err(|e| t!("無法使用全域快捷鍵：{e}", e))?;
        manager.register(key).map_err(|_| t!("「{spec}」已被其他程式使用，請換一組", spec = spec.trim()))?;
        let fired = Arc::new(AtomicBool::new(false));
        let (flag, id) = (fired.clone(), key.id());
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            if e.id == id && e.state == HotKeyState::Pressed {
                flag.store(true, Ordering::SeqCst);
                wake();
            }
        }));
        Ok(Self { manager, key, fired })
    }

    /// True once per press.
    pub fn take(&self) -> bool {
        self.fired.swap(false, Ordering::SeqCst)
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        GlobalHotKeyEvent::set_event_handler(None::<fn(GlobalHotKeyEvent)>);
        let _ = self.manager.unregister(self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_are_parsed_and_bare_keys_refused() {
        assert!(parse(DEFAULT).is_ok());
        assert!(parse(" ctrl+shift+F ").is_ok());
        assert_eq!(parse("Win+Alt+K").unwrap(), parse("Super+Alt+K").unwrap());
        assert!(parse("Space").is_err(), "no modifier");
        assert!(parse("Ctrl+Nope").is_err());
    }

    /// A combination nobody uses, so a running OrangeLink is not disturbed.
    #[test]
    fn a_taken_shortcut_is_reported_and_freed_on_drop() {
        let spec = "Ctrl+Alt+Shift+F24";
        let first = Hotkey::register(spec, || {}).expect("registers");
        assert!(!first.take());
        let second = Hotkey::register(spec, || {}).err().expect("already taken");
        assert!(second.contains("已被其他程式使用"), "{second}");
        drop(first);
        assert!(Hotkey::register(spec, || {}).is_ok(), "free again once dropped");
    }
}
