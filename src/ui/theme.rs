//! The look in one place: fonts, colours, corners, spacing and type sizes.
//!
//! `apply_theme` is the one entry point. It sets up both themes at once: the
//! Catppuccin palette as the base (Mocha for dark, Latte for light), then
//! OrangeLink's own colours on top. Which of the two is shown follows the
//! dark / light / system setting, and switching calls `apply_theme` again.
//!
//! The drawing code uses the named colours below (`accent()`, `star()`, …) and
//! the constants, never raw RGB values, so a change here changes it everywhere.

use catppuccin_egui::{LATTE, MOCHA};
use eframe::egui::{self, vec2, Color32, FontFamily, FontId, Rounding, Stroke, TextStyle};

/// Design tokens of one theme.
#[derive(Clone, Copy)]
pub(crate) struct Tokens {
    // Background layers, from the deepest to the most raised.
    /// Text fields, thumbnail tiles, tracks: below the panels.
    pub bg_sunken: Color32,
    /// Side panels, windows, menus.
    pub bg_panel: Color32,
    /// Raised cards, note boxes, and studio panels.
    pub bg_elevated: Color32,
    /// Alternate rows and other faint highlights.
    pub bg_faint: Color32,
    /// Buttons and other controls at rest.
    pub bg_control: Color32,
    /// A control under the pointer.
    pub bg_hover: Color32,
    /// A control being pressed.
    pub bg_pressed: Color32,
    /// Behind an image or video in the preview: neutral, in both themes, so it
    /// doesn't tint how the picture looks.
    pub bg_viewer: Color32,
    /// Keycap hints (`I`, `O`, `V`).
    pub kbd_bg: Color32,

    // Text, from the strongest to the faintest.
    pub text: Color32,
    pub text_muted: Color32,

    // Lines.
    /// Separators and panel edges.
    pub separator: Color32,
    /// The edge of a control under the pointer, of windows and popups.
    pub outline: Color32,

    // OrangeLink's own colours, on top of the palette.
    /// The orange: active filters, headings, the selected thumbnail's frame.
    pub accent: Color32,
    /// Soft translucent orange wash for active sidebar rows, filter chips, and toggles.
    pub accent_soft: Color32,
    /// Selected list rows and selected text.
    pub selection_bg: Color32,
    pub selection_fg: Color32,
    pub link: Color32,
    pub star: Color32,
    /// Running, done, found.
    pub good: Color32,
    /// Needs a look: unreadable items, a new version, a missing tool.
    pub warning: Color32,
    /// Failed, or about to destroy something.
    pub danger: Color32,
    /// Section labels in the download window; the in / out marks and the
    /// loudness curve of the player.
    pub info: Color32,
}

const fn gray(v: u8) -> Color32 {
    Color32::from_rgb(v, v, v)
}

/// Mocha's layers and contrast steps, in neutral greys: this is a tool for
/// judging pictures, and a blue-violet tint around them (Mocha's background
/// is (30, 30, 46), its text (205, 214, 244)) shifts how their colours look.
/// Each grey matches the lightness of the Mocha colour it replaces.
const DARK: Tokens = Tokens {
    bg_sunken: gray(12),  // Mocha crust (17, 17, 27), a little deeper so thumbnails stand off the panel
    bg_panel: gray(30),   // Mocha base    (30, 30, 46)
    bg_elevated: gray(38),
    bg_faint: gray(36),   // between base and surface 0
    bg_control: gray(50), // Mocha surface0 (49, 50, 68)
    bg_hover: gray(64),   // between surface0 and surface1
    bg_pressed: gray(78), // Mocha surface1 (69, 71, 90), a step lighter
    bg_viewer: gray(16),
    kbd_bg: gray(20),
    text: gray(220),       // Mocha text     (205, 214, 244)
    text_muted: gray(172), // Mocha subtext0 (166, 173, 200)
    separator: gray(46),
    outline: gray(112), // Mocha overlay0 (108, 112, 134)
    accent: Color32::from_rgb(255, 140, 50),
    accent_soft: Color32::from_rgba_premultiplied(42, 23, 8, 42),
    selection_bg: Color32::from_rgb(220, 110, 30),
    selection_fg: Color32::WHITE,
    link: Color32::from_rgb(255, 140, 50),
    star: Color32::from_rgb(255, 196, 0),
    good: Color32::from_rgb(110, 200, 120),
    warning: Color32::from_rgb(250, 180, 60),
    danger: Color32::from_rgb(255, 120, 110),
    info: MOCHA.blue,
};

/// Latte as it is (its background is already close to neutral), with the
/// deeper brand shades that stay readable on white.
const LIGHT_TOKENS: Tokens = Tokens {
    bg_sunken: LATTE.crust,
    bg_panel: LATTE.base,
    bg_elevated: Color32::WHITE,
    bg_faint: LATTE.mantle,
    bg_control: LATTE.surface0,
    bg_hover: LATTE.surface1,
    bg_pressed: LATTE.surface2,
    bg_viewer: gray(16),
    kbd_bg: LATTE.crust,
    text: LATTE.text,
    text_muted: LATTE.subtext0,
    separator: LATTE.surface0,
    outline: LATTE.overlay0,
    accent: Color32::from_rgb(196, 92, 0),
    accent_soft: Color32::from_rgba_premultiplied(25, 12, 0, 32),
    selection_bg: Color32::from_rgb(184, 83, 0),
    selection_fg: Color32::WHITE,
    link: Color32::from_rgb(196, 92, 0),
    star: Color32::from_rgb(190, 128, 0),
    good: Color32::from_rgb(30, 140, 70),
    warning: Color32::from_rgb(176, 104, 0),
    danger: Color32::from_rgb(200, 50, 40),
    info: LATTE.blue,
};

/// Corners of thumbnails, cards, frames and controls; of chips; of windows and menus.
pub(crate) const ROUNDING: f32 = 4.0;
pub(crate) const CHIP_ROUNDING: f32 = 10.0;
const WINDOW_ROUNDING: f32 = 8.0;
const MENU_ROUNDING: f32 = 6.0;

/// Type sizes (points). CJK glyphs need a little more than egui's defaults to stay legible small.
const TEXT_SMALL: f32 = 11.2;
const TEXT_BODY: f32 = 12.5;
const TEXT_HEADING: f32 = 18.0;
const TEXT_MONO: f32 = 12.0;

/// Stars and warnings drawn on the dark pills over thumbnails, in either theme.
pub(crate) const STAR_ON_DARK: Color32 = DARK.star;
pub(crate) const ERROR_ON_DARK: Color32 = DARK.danger;
/// The backing of pills over thumbnails.
pub(crate) const PILL_BG: Color32 = Color32::from_black_alpha(170);
/// Filled buttons with white text (the download window's big buttons): the
/// same in both themes, deep enough for the white to read.
pub(crate) const SOLID_GOOD: Color32 = Color32::from_rgb(40, 140, 70);
pub(crate) const SOLID_INFO: Color32 = Color32::from_rgb(37, 99, 235);
pub(crate) const SOLID_DANGER: Color32 = Color32::from_rgb(180, 60, 60);

/// The light theme is on this frame (set at the start of each frame).
static LIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn set_light_theme(light: bool) {
    LIGHT.store(light, std::sync::atomic::Ordering::Relaxed);
}

/// The tokens of the theme on screen.
pub(crate) fn tokens() -> &'static Tokens {
    if LIGHT.load(std::sync::atomic::Ordering::Relaxed) {
        &LIGHT_TOKENS
    } else {
        &DARK
    }
}

pub(crate) fn accent() -> Color32 {
    tokens().accent
}
pub(crate) fn star() -> Color32 {
    tokens().star
}
pub(crate) fn good() -> Color32 {
    tokens().good
}
pub(crate) fn warning() -> Color32 {
    tokens().warning
}
pub(crate) fn danger() -> Color32 {
    tokens().danger
}
pub(crate) fn info() -> Color32 {
    tokens().info
}

/// The preview is dark in both themes, like a lightbox: its backdrop stays
/// dark so it doesn't change how a picture looks, and everything drawn on it
/// (the top bar, the player controls, the side panel) takes the dark style
/// and tokens to stay readable. Popups opened from it keep the app's theme.
pub(crate) fn in_dark_viewer<R>(ui: &mut egui::Ui, f: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let light = LIGHT.swap(false, std::sync::atomic::Ordering::Relaxed);
    ui.set_style(ui.ctx().style_of(egui::Theme::Dark));
    let out = f(ui);
    LIGHT.store(light, std::sync::atomic::Ordering::Relaxed);
    out
}

/// Windows draws the title bar: make it dark or light with the rest. (winit's
/// own theme switch goes through an undocumented call that Windows 11 no
/// longer follows; this is the documented attribute.)
pub fn set_title_bar_dark(frame: &eframe::Frame, dark: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::HWND,
        Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE},
    };
    let Ok(handle) = frame.window_handle() else { return };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
    let value = windows::core::BOOL::from(dark);
    // SAFETY: the handle is this app's live window; the value is a BOOL of the size given.
    let _ = unsafe {
        DwmSetWindowAttribute(
            HWND(h.hwnd.get() as *mut _),
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const value).cast(),
            std::mem::size_of_val(&value) as u32,
        )
    };
}

/// Fonts once, then both themes' styles. Called at start and whenever the
/// theme setting changes.
pub fn apply_theme(ctx: &egui::Context) {
    load_fonts(ctx);
    for (theme, tokens, palette) in [(egui::Theme::Dark, &DARK, MOCHA), (egui::Theme::Light, &LIGHT_TOKENS, LATTE)] {
        let mut style = (*ctx.style_of(theme)).clone();
        catppuccin_egui::set_style_theme(&mut style, palette);
        brand(&mut style, tokens);
        ctx.set_style_of(theme, style);
    }
}

/// egui's bundled fonts have no CJK glyphs: Windows system fonts are added
/// behind them, for both families. The first is the interface language's
/// (Microsoft JhengHei for Traditional Chinese, Yu Gothic for Japanese…); Malgun
/// Gothic comes after it for Hangul, which the Chinese and Japanese fonts lack.
/// Read from disk again only when the language changes.
fn load_fonts(ctx: &egui::Context) {
    let lang = crate::i18n::current();
    let done = egui::Id::new("orangelink-fonts-loaded");
    if ctx.data(|d| d.get_temp::<crate::i18n::Lang>(done)) == Some(lang) {
        return;
    }
    let mut fonts = egui::FontDefinitions::default();
    let dirs = font_dirs(std::env::var_os("SystemRoot"));
    let read = |names: &[&str]| {
        names.iter().find_map(|name| {
            if name.starts_with('/') {
                return std::fs::read(name).ok();
            }
            dirs.iter().find_map(|d| std::fs::read(d.join(name)).ok())
        })
    };
    let hangul = ["malgun.ttf", "gulim.ttc"];
    let main = read(cjk_fonts(lang));
    let korean = (lang != crate::i18n::Lang::Ko).then(|| read(&hangul)).flatten();
    for (name, bytes) in [("cjk", main), ("hangul", korean)] {
        let Some(bytes) = bytes else { continue };
        fonts.font_data.insert(name.into(), egui::FontData::from_owned(bytes));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.into());
        }
    }
    ctx.set_fonts(fonts);
    ctx.data_mut(|d| d.insert_temp(done, lang));
}

/// The CJK fonts to try for `lang`, best first: each language's own glyph
/// shapes, then the others so any CJK text still shows.
fn cjk_fonts(lang: crate::i18n::Lang) -> &'static [&'static str] {
    use crate::i18n::Lang;
    const LINUX: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
    const MAC: &str = "/System/Library/Fonts/PingFang.ttc";
    match lang {
        Lang::ZhTw | Lang::En => &["msjh.ttc", "msyh.ttc", "mingliu.ttc", "simsun.ttc", "YuGothM.ttc", "meiryo.ttc", MAC, LINUX],
        Lang::ZhCn => &["msyh.ttc", "simsun.ttc", "msjh.ttc", "mingliu.ttc", "YuGothM.ttc", "meiryo.ttc", MAC, LINUX],
        Lang::Ja => &["YuGothM.ttc", "meiryo.ttc", "msgothic.ttc", "msjh.ttc", "msyh.ttc", MAC, LINUX],
        Lang::Ko => &["malgun.ttf", "gulim.ttc", "msjh.ttc", "msyh.ttc", MAC, LINUX],
    }
}

/// Where Windows keeps its fonts: `%SystemRoot%\Fonts` (Windows is not always
/// on C:), then `C:\Windows\Fonts`.
fn font_dirs(system_root: Option<std::ffi::OsString>) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = system_root.filter(|r| !r.is_empty()).map(|r| std::path::PathBuf::from(r).join("Fonts")).into_iter().collect();
    let fallback = std::path::PathBuf::from(r"C:\Windows\Fonts");
    if !dirs.contains(&fallback) {
        dirs.push(fallback);
    }
    dirs
}

/// OrangeLink's layers and colours over the Catppuccin base, and the shared
/// spacing, corners and type sizes.
fn brand(style: &mut egui::Style, t: &Tokens) {
    let v = &mut style.visuals;
    v.panel_fill = t.bg_panel;
    v.window_fill = t.bg_panel;
    v.extreme_bg_color = t.bg_sunken;
    v.code_bg_color = t.bg_sunken;
    v.faint_bg_color = t.bg_faint;
    v.window_stroke.color = t.outline;
    v.hyperlink_color = t.link;
    v.warn_fg_color = t.warning;
    v.error_fg_color = t.danger;
    v.selection.bg_fill = t.selection_bg;
    v.selection.stroke = Stroke::new(1.0_f32, t.selection_fg);
    v.window_rounding = Rounding::same(WINDOW_ROUNDING);
    v.menu_rounding = Rounding::same(MENU_ROUNDING);

    let w = &mut v.widgets;
    // Labels and separators.
    w.noninteractive.bg_fill = t.bg_panel;
    w.noninteractive.weak_bg_fill = t.bg_panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, t.separator);
    w.noninteractive.fg_stroke.color = t.text_muted;
    // Controls: filled, no edge at rest; an edge appears under the pointer.
    for (state, fill, edge) in [
        (&mut w.inactive, t.bg_control, Color32::TRANSPARENT),
        (&mut w.hovered, t.bg_hover, t.outline),
        (&mut w.active, t.bg_pressed, t.outline),
        (&mut w.open, t.bg_control, t.separator),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke.color = edge;
        state.rounding = Rounding::same(ROUNDING);
    }
    w.noninteractive.rounding = Rounding::same(ROUNDING);
    w.inactive.fg_stroke.color = t.text;
    w.hovered.fg_stroke.color = t.text;
    w.active.fg_stroke.color = t.text;
    w.open.fg_stroke.color = t.text;

    let s = &mut style.spacing;
    s.item_spacing = vec2(8.0, 6.0);
    s.button_padding = vec2(8.0, 4.0);
    s.menu_margin = egui::Margin::same(6.0);
    // Thin until the pointer comes near; a little wider than egui's 2 px so it can be found.
    s.scroll.floating_width = 4.0;
    s.scroll.bar_width = 10.0;
    s.scroll.handle_min_length = 24.0;

    style.text_styles = [
        (TextStyle::Small, FontId::new(TEXT_SMALL, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(TEXT_BODY, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(TEXT_BODY, FontFamily::Proportional)),
        (TextStyle::Heading, FontId::new(TEXT_HEADING, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(TEXT_MONO, FontFamily::Monospace)),
    ]
    .into();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn fonts_are_looked_for_where_windows_is_installed() {
        assert_eq!(font_dirs(Some(r"D:\Windows".into())), [PathBuf::from(r"D:\Windows\Fonts"), PathBuf::from(r"C:\Windows\Fonts")]);
        assert_eq!(font_dirs(Some(r"C:\Windows".into())), [PathBuf::from(r"C:\Windows\Fonts")]);
        assert_eq!(font_dirs(None), [PathBuf::from(r"C:\Windows\Fonts")]);
        assert_eq!(font_dirs(Some("".into())), [PathBuf::from(r"C:\Windows\Fonts")]);
    }

    #[test]
    fn each_language_tries_its_own_font_first() {
        use crate::i18n::Lang;
        assert_eq!(cjk_fonts(Lang::ZhTw)[0], "msjh.ttc");
        assert_eq!(cjk_fonts(Lang::ZhCn)[0], "msyh.ttc");
        assert_eq!(cjk_fonts(Lang::Ja)[0], "YuGothM.ttc");
        assert_eq!(cjk_fonts(Lang::Ko)[0], "malgun.ttf");
    }
}
