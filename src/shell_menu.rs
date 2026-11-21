//! The Windows right-click menu for files (the classic one Explorer shows under
//! "Show more options"), with everything other programs add: 7-Zip, Open with,
//! Send to, Properties, Git and so on.
//!
//! Adapted from Prime Files (`crates/nohrs-pages/src/shell_menu.rs`, `show`),
//! MIT License, Copyright (c) 2025 Nohrs contributors, (c) 2026 Prime Files contributors.

use std::path::PathBuf;

/// Pops the menu up at the cursor and runs what is picked; files may sit in
/// different folders (each copied item has its own). Runs a modal loop until the
/// menu closes.
#[cfg(windows)]
pub fn show(paths: &[PathBuf], dark: bool) -> Result<(), String> {
    if paths.is_empty() {
        return Err("沒有檔案".into());
    }
    // SAFETY: called from the UI thread (egui's), with paths that are not empty.
    unsafe { platform::show(paths, dark) }.map_err(|e| e.message())
}

#[cfg(not(windows))]
pub fn show(_paths: &[PathBuf], _dark: bool) -> Result<(), String> {
    Err("只有 Windows 有這個選單".into())
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    //! None of the Shell's menu interfaces has a safe binding.

    use std::{cell::RefCell, path::PathBuf, sync::OnceLock};
    use windows::{
        core::{Interface, Result, PCSTR, PCWSTR},
        Win32::{
            Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
            System::{
                Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED},
                LibraryLoader::{GetProcAddress, LoadLibraryW},
            },
            UI::{
                Input::KeyboardAndMouse::GetActiveWindow,
                Shell::{
                    Common::ITEMIDLIST, DefSubclassProc, ILFindLastID, IContextMenu, IContextMenu3, IShellFolder, RemoveWindowSubclass, SHBindToParent, SHGetDesktopFolder,
                    SHParseDisplayName, SetWindowSubclass, CMF_EXPLORE, CMF_NORMAL, CMIC_MASK_PTINVOKE, CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX,
                },
                WindowsAndMessaging::{
                    CreatePopupMenu, DestroyMenu, GetCursorPos, TrackPopupMenuEx, HMENU, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_DRAWITEM,
                    WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR,
                },
            },
        },
    };

    const FIRST_ID: u32 = 1;
    const LAST_ID: u32 = 0x7FFF;
    const SUBCLASS_ID: usize = 0x4F4C_4D4E;
    /// `CMIC_MASK_UNICODE` (shobjidl.h), missing from the bindings.
    const CMIC_MASK_UNICODE: u32 = 0x4000;

    thread_local! {
        // The menu whose owner-drawn submenus ("Open with", "Send to", "New") are being
        // filled: those arrive as messages to the owner window while the popup is open.
        static OPEN_MENU: RefCell<Option<IContextMenu3>> = const { RefCell::new(None) };
    }

    pub(super) struct Menu(pub(super) HMENU);
    impl Drop for Menu {
        fn drop(&mut self) {
            // SAFETY: the menu is this value's, made by CreatePopupMenu, destroyed once.
            let _ = unsafe { DestroyMenu(self.0) };
        }
    }

    /// A Shell item id list, freed on drop.
    struct Pidl(*mut ITEMIDLIST);
    impl Pidl {
        fn parse(path: &std::path::Path) -> Result<Self> {
            // The Shell's parser takes `\` only.
            let w = wide(&path.to_string_lossy().replace('/', "\\"));
            let mut pidl = std::ptr::null_mut();
            // SAFETY: `w` is NUL-terminated and outlives the call; `pidl` is ours to free.
            unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None) }?;
            Ok(Self(pidl))
        }
    }
    impl Drop for Pidl {
        fn drop(&mut self) {
            // SAFETY: the list was allocated by the Shell for us, freed once (null is allowed).
            unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
        }
    }

    /// The Shell's menu object for these files, and the menu it filled.
    ///
    /// # Safety
    /// Call on a thread that is (or may become) a COM single-threaded apartment,
    /// such as the UI thread; `hwnd` is a live window of this thread, or null.
    pub(super) unsafe fn build(hwnd: HWND, paths: &[PathBuf]) -> Result<(IContextMenu, Menu)> {
        // Shell extensions expect an STA, which the UI thread already is (OLE drag and drop).
        // SAFETY: no pointers; a thread already in its apartment just gets an error code.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let pidls = paths.iter().map(|p| Pidl::parse(p)).collect::<Result<Vec<_>>>()?;
        let first = pidls.first().ok_or_else(windows::core::Error::empty)?;
        let one_folder = paths.iter().all(|p| p.parent() == paths[0].parent());
        let shell: IContextMenu = if one_folder {
            // SAFETY: every id list is valid until `pidls` drops at the end of this
            // function, after the Shell has copied what it keeps; ILFindLastID points
            // inside the same list.
            let parent: IShellFolder = unsafe { SHBindToParent(first.0, None) }?;
            let children: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| unsafe { ILFindLastID(p.0) } as *const ITEMIDLIST).collect();
            unsafe { parent.GetUIObjectOf(hwnd, &children, None) }?
        } else {
            // Files from several folders: full id lists are relative to the desktop, which
            // takes them all at once, as for search results in Explorer.
            // SAFETY: as above; the full id lists live as long as `pidls`.
            let desktop = unsafe { SHGetDesktopFolder() }?;
            let all: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| p.0 as *const ITEMIDLIST).collect();
            unsafe { desktop.GetUIObjectOf(hwnd, &all, None) }?
        };
        // SAFETY: the new menu is owned by `Menu`; the Shell fills it within the id range given.
        let menu = Menu(unsafe { CreatePopupMenu() }?);
        unsafe { shell.QueryContextMenu(menu.0, 0, FIRST_ID, LAST_ID, CMF_NORMAL | CMF_EXPLORE) }.ok()?;
        Ok((shell, menu))
    }

    /// # Safety
    /// As for [`build`]: on the UI thread, with at least one path.
    pub unsafe fn show(paths: &[PathBuf], dark: bool) -> Result<()> {
        // SAFETY: no arguments; the window it returns (or null) belongs to this thread.
        let hwnd = unsafe { GetActiveWindow() };
        set_menu_theme(dark);
        // SAFETY: this function's own contract, and `hwnd` is this thread's window or null.
        let (shell, menu) = unsafe { build(hwnd, paths) }?;

        let mut point = POINT::default();
        // SAFETY: writes one POINT we own.
        unsafe { GetCursorPos(&mut point) }?;
        let forwarding = shell.cast::<IContextMenu3>().ok();
        // SAFETY: the subclass procedure is a plain function with the right signature,
        // and it is removed again below before this function returns.
        let subclassed = forwarding.is_some() && unsafe { SetWindowSubclass(hwnd, Some(forward_menu_messages), SUBCLASS_ID, 0) }.as_bool();
        OPEN_MENU.with(|open| *open.borrow_mut() = forwarding);
        // SAFETY: the menu and window are live for the whole modal loop.
        let id = unsafe { TrackPopupMenuEx(menu.0, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, point.x, point.y, hwnd, None) }.0 as u32;
        OPEN_MENU.with(|open| *open.borrow_mut() = None);
        if subclassed {
            // SAFETY: the same window, procedure and id as were set above.
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(forward_menu_messages), SUBCLASS_ID) };
        }
        if id < FIRST_ID {
            return Ok(()); // dismissed
        }

        let offset = (id - FIRST_ID) as usize;
        let folder = paths[0].parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        // The ANSI field is read as the system code page, not UTF-8: for a folder
        // with other letters, an old non-Unicode handler gets none rather than garbage.
        let folder_ansi: Vec<u8> = if folder.is_ascii() { folder.bytes().chain(std::iter::once(0)).collect() } else { Vec::new() };
        let folder_wide = wide(&folder);
        let info = CMINVOKECOMMANDINFOEX {
            cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            fMask: CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE,
            hwnd,
            // The command's offset where a verb string would go, as MAKEINTRESOURCE does.
            lpVerb: PCSTR(offset as *const u8),
            lpVerbW: PCWSTR(offset as *const u16),
            lpDirectory: if folder_ansi.is_empty() { PCSTR::null() } else { PCSTR(folder_ansi.as_ptr()) },
            lpDirectoryW: PCWSTR(folder_wide.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ptInvoke: point,
            ..Default::default()
        };
        // SAFETY: an EX structure whose cbSize says so, which InvokeCommand accepts in
        // place of the short one; the folder strings it points to are NUL-terminated
        // and live until the call returns. The verb is a command offset, not a pointer.
        unsafe { shell.InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const CMINVOKECOMMANDINFO) }
    }

    unsafe extern "system" fn forward_menu_messages(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
        if matches!(message, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
            let handled = OPEN_MENU.with(|open| {
                let open = open.borrow();
                let menu = open.as_ref()?;
                let mut result = LRESULT(0);
                // SAFETY: the message is passed on as Windows gave it; `result` is ours.
                unsafe { menu.HandleMenuMsg2(message, wparam, lparam, Some(&mut result)) }.ok().map(|()| result)
            });
            if let Some(result) = handled {
                return result;
            }
        }
        // SAFETY: the usual hand-on to the next procedure, with the message as given.
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    /// Dark or light popups to match the app: the uxtheme exports (by ordinal) Explorer
    /// uses, present since Windows 10 1903. Without them the menu is simply light.
    fn set_menu_theme(dark: bool) {
        type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
        type FlushMenuThemes = unsafe extern "system" fn();
        static EXPORTS: OnceLock<Option<(SetPreferredAppMode, FlushMenuThemes)>> = OnceLock::new();
        // SAFETY: uxtheme stays loaded (it is never freed). Ordinals 135 and 136 are
        // SetPreferredAppMode(int) -> int and FlushMenuThemes(), the signatures they are
        // cast to, in every Windows 10 1903+ and 11 build; this app needs Windows 10 or later.
        let exports = EXPORTS.get_or_init(|| unsafe {
            let module = LoadLibraryW(windows::core::w!("uxtheme.dll")).ok()?;
            let set_mode = GetProcAddress(module, PCSTR(135 as *const u8))?;
            let flush = GetProcAddress(module, PCSTR(136 as *const u8))?;
            Some((
                std::mem::transmute::<unsafe extern "system" fn() -> isize, SetPreferredAppMode>(set_mode),
                std::mem::transmute::<unsafe extern "system" fn() -> isize, FlushMenuThemes>(flush),
            ))
        });
        if let Some((set_mode, flush)) = exports {
            // 2 = force dark, 3 = force light.
            // SAFETY: the functions found above, called with the arguments they take.
            unsafe {
                set_mode(if dark { 2 } else { 3 });
                flush();
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(all(test, windows))]
#[allow(unsafe_code)]
mod tests {
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{GetMenuItemCount, GetMenuStringW, MF_BYPOSITION},
    };

    fn labels(paths: &[std::path::PathBuf]) -> Vec<String> {
        // SAFETY: the test thread may become an STA, with no window; the menu lives
        // until the end of this function and each label is read into a buffer of the size given.
        let (_shell, menu) = unsafe { super::platform::build(HWND::default(), paths) }.expect("a menu");
        let n = unsafe { GetMenuItemCount(Some(menu.0)) };
        (0..n)
            .map(|i| {
                let mut buf = [0u16; 256];
                let len = unsafe { GetMenuStringW(menu.0, i as u32, Some(&mut buf), MF_BYPOSITION) };
                String::from_utf16_lossy(&buf[..len.max(0) as usize])
            })
            .filter(|s| !s.is_empty())
            .collect()
    }

    #[test]
    fn menus_for_one_file_and_files_in_two_folders() {
        let dir = std::env::temp_dir().join(format!("orangelink-menu-{}", crate::model::new_id()));
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        let (a, b) = (dir.join("a").join("one.txt"), dir.join("b").join("two.txt"));
        std::fs::write(&a, "1").unwrap();
        std::fs::write(&b, "2").unwrap();
        let one = labels(std::slice::from_ref(&a));
        println!("one: {one:?}");
        let two = labels(&[a, b]);
        println!("two: {two:?}");
        // Every file menu ends with Properties; its label depends on the system language.
        assert!(one.len() >= 3 && two.len() >= 3, "{one:?} / {two:?}");
        assert_eq!(one.last(), two.last(), "Properties in both");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
