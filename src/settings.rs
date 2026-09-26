use crate::app::IDM_EXIT;
use crate::config::{
    open_config_editor, parse_hotkeys, read_ini_values, write_ini_values, IniValues,
    SWITCH_APPS_HOTKEY_ID, SWITCH_WINDOWS_HOTKEY_ID,
};
use crate::utils::{get_window_user_data, set_window_user_data};

use anyhow::{anyhow, Result};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, GetSysColorBrush, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    COLOR_BTNFACE, DEFAULT_CHARSET, DEFAULT_PITCH, HFONT, OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::Win32::UI::Controls::{BST_CHECKED, BST_UNCHECKED};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetMessageW, GetSystemMetrics, IsDialogMessageW, IsWindow, LoadCursorW, LoadIconW, MessageBoxW,
    PostMessageW, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowPos, ShowWindow,
    TranslateMessage, BM_GETCHECK, BM_SETCHECK, BN_CLICKED, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON,
    BS_GROUPBOX, BS_PUSHBUTTON, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL,
    ES_AUTOHSCROLL, HMENU, IDC_ARROW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION,
    MB_OK, MB_YESNO, MSG, SM_CXSCREEN, SM_CYSCREEN, SWP_NOMOVE, SWP_NOZORDER, SW_SHOWNORMAL,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_GETTEXT, WM_GETTEXTLENGTH,
    WM_QUIT, WM_SETFONT, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
    WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

const CLASS_NAME: PCWSTR = w!("WindowSwitcherSettings");
const TITLE: PCWSTR = w!("Window Switcher Settings");
/// Icon resource id from `assets/app.rc`.
const APP_ICON: PCWSTR = PCWSTR(0x101 as *const u16);

const IDC_SAVE: usize = 1; // IDOK: pressing Enter triggers Save
const IDC_CANCEL: usize = 2; // IDCANCEL: pressing Esc closes the window
const IDC_OPEN_FILE: usize = 3;

const IDC_TRAYICON: i32 = 100;
const IDC_SW_HOTKEY: i32 = 201;
const IDC_SW_BLACKLIST: i32 = 202;
const IDC_SW_IGNORE_MIN: i32 = 203;
const IDC_SW_DESKTOP: i32 = 204;
const IDC_SW_MERGE: i32 = 205;
const IDC_SA_ENABLE: i32 = 301;
const IDC_SA_HOTKEY: i32 = 302;
const IDC_SA_IGNORE_MIN: i32 = 303;
const IDC_SA_DESKTOP: i32 = 304;
const IDC_SA_ICONS: i32 = 305;
const IDC_LOG_LEVEL: i32 = 401;
const IDC_LOG_PATH: i32 = 402;

const DESKTOP_ITEMS: [&str; 3] = [
    "Auto (match Alt-Tab)",
    "Only current desktop",
    "All desktops",
];
const LOG_LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];

const ERROR_CLASS_ALREADY_EXISTS: u32 = 1416;
const CLIENT_WIDTH: i32 = 452;

/// Set while the settings window is open so the keyboard hook passes keys
/// through to the system instead of swallowing the switch hotkeys.
pub static SETTINGS_OPEN: AtomicBool = AtomicBool::new(false);

static SETTINGS_HWND: AtomicIsize = AtomicIsize::new(0);
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

struct SettingsState {
    hwnd: HWND,
    /// The main app window; `None` when running standalone (`settings` subcommand).
    owner: Option<HWND>,
    trayicon: HWND,
    sw_hotkey: HWND,
    sw_blacklist: HWND,
    sw_ignore_min: HWND,
    sw_desktop: HWND,
    sw_merge: HWND,
    sa_enable: HWND,
    sa_hotkey: HWND,
    sa_ignore_min: HWND,
    sa_desktop: HWND,
    sa_icons: HWND,
    log_level: HWND,
    log_path: HWND,
}

/// Open the settings window and run a modal message loop until it is closed.
///
/// When `owner` is `Some`, this loop is nested inside the app's message loop;
/// a `WM_QUIT` received while open is re-posted so the app still exits.
pub fn open(owner: Option<HWND>) -> Result<()> {
    unsafe {
        let existing = SETTINGS_HWND.load(Ordering::SeqCst);
        if existing != 0 && IsWindow(Some(HWND(existing as _))).as_bool() {
            let hwnd = HWND(existing as _);
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
            return Ok(());
        }

        let values = read_ini_values()?;
        let dpi = GetDpiForSystem();
        let font = create_font(dpi);
        let hwnd = match create_window(owner, dpi, &font, &values) {
            Ok(hwnd) => hwnd,
            Err(err) => {
                let _ = DeleteObject(font.into());
                return Err(err);
            }
        };

        SETTINGS_OPEN.store(true, Ordering::SeqCst);

        let mut message = MSG::default();
        let mut quit_pending = false;
        loop {
            let ret = GetMessageW(&mut message, None, 0, 0);
            match ret.0 {
                -1 => {
                    SETTINGS_OPEN.store(false, Ordering::SeqCst);
                    let _ = DeleteObject(font.into());
                    return Err(anyhow!("Failed to get message, {:?}", GetLastError()));
                }
                0 => {
                    quit_pending = true;
                    break;
                }
                _ => {
                    if !IsDialogMessageW(hwnd, &message).as_bool() {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
            }
            if !IsWindow(Some(hwnd)).as_bool() {
                break;
            }
        }

        SETTINGS_OPEN.store(false, Ordering::SeqCst);
        let _ = DeleteObject(font.into());
        if quit_pending && owner.is_some() {
            // The app asked to quit while settings were open (e.g. tray Exit);
            // re-post so the outer message loop sees it too.
            let _ = PostMessageW(None, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        Ok(())
    }
}

fn create_font(dpi: u32) -> HFONT {
    unsafe {
        CreateFontW(
            -(dpi as i32 * 9 / 72), // 9pt, scaled
            0,
            0,
            0,
            400, // FW_NORMAL
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        )
    }
}

unsafe fn create_window(
    owner: Option<HWND>,
    dpi: u32,
    font: &HFONT,
    values: &IniValues,
) -> Result<HWND> {
    register_class()?;

    let scale = |v: i32| v * dpi as i32 / 96;
    let style = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0);

    // Create the window with a rough height first; once the controls are laid
    // out the frame is resized to fit — before the window is shown.
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(CLIENT_WIDTH),
        bottom: scale(620),
    };
    let _ = AdjustWindowRect(&mut rect, style, false);
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let x = (GetSystemMetrics(SM_CXSCREEN) - width).max(0) / 2;
    let y = (GetSystemMetrics(SM_CYSCREEN) - height).max(0) / 3;

    let hwnd = CreateWindowExW(
        Default::default(),
        CLASS_NAME,
        TITLE,
        style,
        x,
        y,
        width,
        height,
        owner,
        None,
        None,
        None,
    )
    .map_err(|err| anyhow!("Failed to create settings window, {err}"))?;

    let content_height = create_controls(hwnd, owner, dpi, font, values)?;

    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(CLIENT_WIDTH),
        bottom: content_height,
    };
    let _ = AdjustWindowRect(&mut rect, style, false);
    let _ = SetWindowPos(
        hwnd,
        None,
        0,
        0,
        rect.right - rect.left,
        rect.bottom - rect.top,
        SWP_NOMOVE | SWP_NOZORDER,
    );

    SETTINGS_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
    let _ = SetForegroundWindow(hwnd);
    Ok(hwnd)
}

unsafe fn register_class() -> Result<()> {
    if CLASS_REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }
    let hinstance =
        GetModuleHandleW(None).map_err(|err| anyhow!("Failed to get module handle, {err}"))?;
    let hcursor =
        LoadCursorW(None, IDC_ARROW).map_err(|err| anyhow!("Failed to load cursor, {err}"))?;
    let hicon = LoadIconW(Some(hinstance.into()), APP_ICON).unwrap_or_default();
    let wc = WNDCLASSW {
        lpfnWndProc: Some(settings_proc),
        hInstance: hinstance.into(),
        lpszClassName: CLASS_NAME,
        hCursor: hcursor,
        hIcon: hicon,
        hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
        ..Default::default()
    };
    let atom = RegisterClassW(&wc);
    if atom == 0 {
        let err = GetLastError();
        if err.0 != ERROR_CLASS_ALREADY_EXISTS {
            return Err(anyhow!("Failed to register settings window class, {err:?}"));
        }
    }
    CLASS_REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[allow(clippy::too_many_arguments)]
unsafe fn child(
    parent: HWND,
    font: &HFONT,
    class: PCWSTR,
    text: &str,
    ex_style: WINDOW_EX_STYLE,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: i32,
) -> Result<HWND> {
    let wide = to_wide(text);
    let hwnd = CreateWindowExW(
        ex_style,
        class,
        PCWSTR(wide.as_ptr()),
        style,
        x,
        y,
        width,
        height,
        Some(parent),
        Some(HMENU(id as _)),
        None,
        None,
    )
    .map_err(|err| anyhow!("Failed to create control, {err}"))?;
    let _ = SendMessageW(
        hwnd,
        WM_SETFONT,
        Some(WPARAM(font.0 as usize)),
        Some(LPARAM(1)),
    );
    Ok(hwnd)
}

unsafe fn create_controls(
    hwnd: HWND,
    owner: Option<HWND>,
    dpi: u32,
    font: &HFONT,
    values: &IniValues,
) -> Result<i32> {
    let s = |v: i32| v * dpi as i32 / 96;

    let client_w = s(CLIENT_WIDTH);
    let pad_x = s(16);
    let label_w = s(128);
    let ctrl_x = pad_x + label_w;
    let ctrl_w = client_w - ctrl_x - pad_x;

    let label = |text: &str, x: i32, y: i32, w: i32| -> Result<HWND> {
        child(
            hwnd,
            font,
            w!("STATIC"),
            text,
            WINDOW_EX_STYLE(0),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
            x,
            y,
            w,
            s(20),
            -1,
        )
    };
    let edit = |text: &str, x: i32, y: i32, w: i32, id: i32| -> Result<HWND> {
        child(
            hwnd,
            font,
            w!("EDIT"),
            text,
            WS_EX_CLIENTEDGE,
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32,
            ),
            x,
            y,
            w,
            s(23),
            id,
        )
    };
    let checkbox = |text: &str, x: i32, y: i32, w: i32, checked: bool, id: i32| -> Result<HWND> {
        let hwnd = child(
            hwnd,
            font,
            w!("BUTTON"),
            text,
            WINDOW_EX_STYLE(0),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32),
            x,
            y,
            w,
            s(22),
            id,
        )?;
        let state = if checked {
            BST_CHECKED.0
        } else {
            BST_UNCHECKED.0
        };
        let _ = SendMessageW(hwnd, BM_SETCHECK, Some(WPARAM(state as usize)), None);
        Ok(hwnd)
    };
    let combo =
        |x: i32, y: i32, w: i32, items: &[&str], selected: usize, id: i32| -> Result<HWND> {
            let hwnd = child(
                hwnd,
                font,
                w!("COMBOBOX"),
                "",
                WINDOW_EX_STYLE(0),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32),
                x,
                y,
                w,
                s(120),
                id,
            )?;
            for item in items {
                let wide = to_wide(item);
                let _ = SendMessageW(
                    hwnd,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(wide.as_ptr() as isize)),
                );
            }
            let _ = SendMessageW(hwnd, CB_SETCURSEL, Some(WPARAM(selected)), None);
            Ok(hwnd)
        };
    let group = |text: &str, y: i32| -> Result<HWND> {
        child(
            hwnd,
            font,
            w!("BUTTON"),
            text,
            WINDOW_EX_STYLE(0),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | BS_GROUPBOX as u32),
            s(10),
            y,
            client_w - s(20),
            s(8), // resized once the content height is known
            -1,
        )
    };

    let desktop_index = |v: Option<bool>| match v {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    };

    let mut y = s(12);

    let trayicon = checkbox(
        "Show tray icon",
        pad_x,
        y,
        client_w - pad_x * 2,
        values.trayicon,
        IDC_TRAYICON,
    )?;
    y += s(34);

    // [switch-windows]
    let sw_group = group("Switch Windows", y)?;
    let mut iy = y + s(24);
    label("Hotkey(s)", pad_x, iy + s(3), label_w)?;
    let sw_hotkey = edit(
        &values.switch_windows_hotkey,
        ctrl_x,
        iy,
        ctrl_w,
        IDC_SW_HOTKEY,
    )?;
    iy += s(29);
    label("Blacklist", pad_x, iy + s(3), label_w)?;
    let sw_blacklist = edit(
        &values.switch_windows_blacklist,
        ctrl_x,
        iy,
        ctrl_w,
        IDC_SW_BLACKLIST,
    )?;
    iy += s(29);
    let sw_ignore_min = checkbox(
        "Ignore minimized windows",
        ctrl_x,
        iy,
        ctrl_w,
        values.switch_windows_ignore_minimal,
        IDC_SW_IGNORE_MIN,
    )?;
    iy += s(27);
    label("Desktops", pad_x, iy + s(3), label_w)?;
    let sw_desktop = combo(
        ctrl_x,
        iy,
        ctrl_w,
        &DESKTOP_ITEMS,
        desktop_index(values.switch_windows_only_current_desktop),
        IDC_SW_DESKTOP,
    )?;
    iy += s(29);
    let sw_merge = checkbox(
        "Merge browser profiles (Chrome/Edge)",
        ctrl_x,
        iy,
        ctrl_w,
        values.switch_windows_merge_browser_profiles,
        IDC_SW_MERGE,
    )?;
    iy += s(26);
    resize_group(sw_group, s(10), y, client_w - s(20), iy - y + s(4));
    y = iy + s(10);

    // [switch-apps]
    let sa_group = group("Switch Apps", y)?;
    let mut iy = y + s(24);
    let sa_enable = checkbox(
        "Enable switching apps (replaces Alt+Tab)",
        ctrl_x,
        iy,
        ctrl_w,
        values.switch_apps_enable,
        IDC_SA_ENABLE,
    )?;
    iy += s(27);
    label("Hotkey(s)", pad_x, iy + s(3), label_w)?;
    let sa_hotkey = edit(
        &values.switch_apps_hotkey,
        ctrl_x,
        iy,
        ctrl_w,
        IDC_SA_HOTKEY,
    )?;
    iy += s(29);
    let sa_ignore_min = checkbox(
        "Ignore minimized windows",
        ctrl_x,
        iy,
        ctrl_w,
        values.switch_apps_ignore_minimal,
        IDC_SA_IGNORE_MIN,
    )?;
    iy += s(27);
    label("Desktops", pad_x, iy + s(3), label_w)?;
    let sa_desktop = combo(
        ctrl_x,
        iy,
        ctrl_w,
        &DESKTOP_ITEMS,
        desktop_index(values.switch_apps_only_current_desktop),
        IDC_SA_DESKTOP,
    )?;
    iy += s(29);
    label("Override icons", pad_x, iy + s(3), label_w)?;
    let sa_icons = edit(
        &values.switch_apps_override_icons,
        ctrl_x,
        iy,
        ctrl_w,
        IDC_SA_ICONS,
    )?;
    iy += s(26);
    resize_group(sa_group, s(10), y, client_w - s(20), iy - y + s(4));
    y = iy + s(10);

    // [log]
    let log_group = group("Log", y)?;
    let mut iy = y + s(24);
    label("Level", pad_x, iy + s(3), label_w)?;
    let log_level_index = LOG_LEVELS
        .iter()
        .position(|v| *v == values.log_level)
        .unwrap_or(3);
    let log_level = combo(
        ctrl_x,
        iy,
        s(120),
        &LOG_LEVELS,
        log_level_index,
        IDC_LOG_LEVEL,
    )?;
    iy += s(29);
    label("File", pad_x, iy + s(3), label_w)?;
    let log_path = edit(&values.log_path, ctrl_x, iy, ctrl_w, IDC_LOG_PATH)?;
    iy += s(26);
    resize_group(log_group, s(10), y, client_w - s(20), iy - y + s(4));
    y = iy + s(12);

    // hints
    label(
        "Hotkey format: modifier+key, e.g. alt+` — separate multiple hotkeys with ||",
        pad_x,
        y,
        client_w - pad_x * 2,
    )?;
    y += s(18);
    label(
        "Blacklist: comma-separated exe list.  Override icons: app.exe=icon.ico,…",
        pad_x,
        y,
        client_w - pad_x * 2,
    )?;
    y += s(26);

    // buttons
    let button = |text: &str, x: i32, y: i32, w: i32, default: bool, id: usize| -> Result<HWND> {
        let bs = if default {
            BS_DEFPUSHBUTTON as u32
        } else {
            BS_PUSHBUTTON as u32
        };
        child(
            hwnd,
            font,
            w!("BUTTON"),
            text,
            WINDOW_EX_STYLE(0),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | bs),
            x,
            y,
            w,
            s(28),
            id as i32,
        )
    };
    button("Save", pad_x, y, s(96), true, IDC_SAVE)?;
    button("Cancel", pad_x + s(104), y, s(88), false, IDC_CANCEL)?;
    button(
        "Open Config File…",
        pad_x + s(200),
        y,
        s(160),
        false,
        IDC_OPEN_FILE,
    )?;
    y += s(28) + s(12);

    let state = Box::new(SettingsState {
        hwnd,
        owner,
        trayicon,
        sw_hotkey,
        sw_blacklist,
        sw_ignore_min,
        sw_desktop,
        sw_merge,
        sa_enable,
        sa_hotkey,
        sa_ignore_min,
        sa_desktop,
        sa_icons,
        log_level,
        log_path,
    });
    set_window_user_data(hwnd, Box::into_raw(state) as isize);

    debug!("settings controls layout height {y}");
    Ok(y)
}

unsafe fn resize_group(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER);
}

unsafe extern "system" fn settings_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = wparam.0 & 0xffff;
            let code = ((wparam.0 >> 16) & 0xffff) as u32;
            if code == BN_CLICKED {
                match id {
                    IDC_SAVE => {
                        if let Some(state) = get_state(hwnd) {
                            state.save();
                        }
                        return LRESULT(0);
                    }
                    IDC_CANCEL => {
                        let _ = DestroyWindow(hwnd);
                        return LRESULT(0);
                    }
                    IDC_OPEN_FILE => {
                        if let Err(err) = open_config_editor() {
                            error_box(hwnd, &err.to_string());
                        }
                        return LRESULT(0);
                    }
                    _ => {}
                }
            }
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            return LRESULT(0);
        }
        WM_DESTROY => {
            SETTINGS_HWND.store(0, Ordering::SeqCst);
            if let Some(ptr) = take_state(hwnd) {
                drop(Box::from_raw(ptr));
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

impl SettingsState {
    fn save(&mut self) {
        unsafe {
            let sw_hotkey = get_text(self.sw_hotkey);
            if let Err(err) =
                parse_hotkeys(SWITCH_WINDOWS_HOTKEY_ID, "switch windows", sw_hotkey.trim())
            {
                error_box(
                    self.hwnd,
                    &format!(
                        "{err}\n\nFormat: modifier+key, e.g. alt+` (multiple: alt+` || ctrl+q)"
                    ),
                );
                let _ = SetFocus(Some(self.sw_hotkey));
                return;
            }
            let sa_hotkey = get_text(self.sa_hotkey);
            if let Err(err) = parse_hotkeys(SWITCH_APPS_HOTKEY_ID, "switch apps", sa_hotkey.trim())
            {
                error_box(
                    self.hwnd,
                    &format!(
                        "{err}\n\nFormat: modifier+key, e.g. alt+tab (multiple: alt+tab || ctrl+tab)"
                    ),
                );
                let _ = SetFocus(Some(self.sa_hotkey));
                return;
            }

            let values = IniValues {
                trayicon: is_checked(self.trayicon),
                switch_windows_hotkey: sw_hotkey.trim().to_string(),
                switch_windows_blacklist: get_text(self.sw_blacklist).trim().to_string(),
                switch_windows_ignore_minimal: is_checked(self.sw_ignore_min),
                switch_windows_only_current_desktop: desktop_value(self.sw_desktop),
                switch_windows_merge_browser_profiles: is_checked(self.sw_merge),
                switch_apps_enable: is_checked(self.sa_enable),
                switch_apps_hotkey: sa_hotkey.trim().to_string(),
                switch_apps_ignore_minimal: is_checked(self.sa_ignore_min),
                switch_apps_only_current_desktop: desktop_value(self.sa_desktop),
                switch_apps_override_icons: get_text(self.sa_icons).trim().to_string(),
                log_level: LOG_LEVELS
                    .get(combo_index(self.log_level).unwrap_or(3))
                    .unwrap_or(&"info")
                    .to_string(),
                log_path: get_text(self.log_path).trim().to_string(),
            };

            match write_ini_values(&values) {
                Ok(_) => {
                    if let Some(owner) = self.owner {
                        let ret = MessageBoxW(
                            Some(self.hwnd),
                            w!("Config saved.\n\nRestart Window Switcher now to apply the changes?"),
                            TITLE,
                            MB_YESNO | MB_ICONQUESTION,
                        );
                        if ret == IDYES {
                            if let Err(err) = restart_app() {
                                error_box(self.hwnd, &format!("Failed to restart, {err}"));
                                return;
                            }
                            let _ = DestroyWindow(self.hwnd);
                            let _ = PostMessageW(
                                Some(owner),
                                WM_COMMAND,
                                WPARAM(IDM_EXIT as usize),
                                LPARAM(0),
                            );
                        }
                    } else {
                        let _ = MessageBoxW(
                            Some(self.hwnd),
                            w!("Config saved."),
                            TITLE,
                            MB_OK | MB_ICONINFORMATION,
                        );
                    }
                }
                Err(err) => error_box(self.hwnd, &format!("Failed to save config, {err}")),
            }
        }
    }
}

fn desktop_value(hwnd: HWND) -> Option<bool> {
    match unsafe { combo_index(hwnd) } {
        Some(1) => Some(true),
        Some(2) => Some(false),
        _ => None,
    }
}

unsafe fn is_checked(hwnd: HWND) -> bool {
    SendMessageW(hwnd, BM_GETCHECK, None, None).0 == BST_CHECKED.0 as isize
}

unsafe fn combo_index(hwnd: HWND) -> Option<usize> {
    let index = SendMessageW(hwnd, CB_GETCURSEL, None, None).0;
    if index < 0 {
        None
    } else {
        Some(index as usize)
    }
}

unsafe fn get_text(hwnd: HWND) -> String {
    let len = SendMessageW(hwnd, WM_GETTEXTLENGTH, None, None).0 as usize;
    let mut buf = vec![0u16; len + 1];
    SendMessageW(
        hwnd,
        WM_GETTEXT,
        Some(WPARAM(buf.len())),
        Some(LPARAM(buf.as_mut_ptr() as isize)),
    );
    String::from_utf16_lossy(&buf[..len])
}

fn error_box(hwnd: HWND, text: &str) {
    let wide_text = to_wide(text);
    unsafe {
        let _ = MessageBoxW(
            Some(hwnd),
            PCWSTR(wide_text.as_ptr()),
            TITLE,
            MB_OK | MB_ICONERROR,
        );
    }
}

fn restart_app() -> Result<()> {
    use std::os::windows::process::CommandExt;
    let exe = std::env::current_exe().map_err(|err| anyhow!("Failed to locate the exe, {err}"))?;
    // The new instance waits for this one to exit and release the
    // single-instance mutex (see `--restart` in main.rs). Spawning the exe
    // directly — instead of going through cmd — keeps the elevation level and
    // avoids cmd's quote handling mangling paths with spaces.
    Command::new(exe)
        .arg("--restart")
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .map_err(|err| anyhow!("Failed to restart, {err}"))?;
    Ok(())
}

unsafe fn get_state(hwnd: HWND) -> Option<&'static mut SettingsState> {
    let ptr = get_window_user_data(hwnd);
    if ptr == 0 {
        None
    } else {
        Some(&mut *(ptr as *mut SettingsState))
    }
}

unsafe fn take_state(hwnd: HWND) -> Option<*mut SettingsState> {
    let ptr = get_window_user_data(hwnd);
    if ptr == 0 {
        None
    } else {
        set_window_user_data(hwnd, 0);
        Some(ptr as *mut SettingsState)
    }
}
