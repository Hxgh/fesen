use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ptr,
    sync::{Mutex, OnceLock},
};
use tauri::{Emitter, Manager, WebviewWindow};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{BeginPaint, EndPaint, GetStockObject, NULL_BRUSH, PAINTSTRUCT, PtInRect},
    System::{
        LibraryLoader::{GetModuleHandleW, GetProcAddress},
        SystemInformation::OSVERSIONINFOW,
    },
    UI::{
        Input::KeyboardAndMouse::{
            IsWindowEnabled, ReleaseCapture, SetCapture, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
            TrackMouseEvent,
        },
        WindowsAndMessaging::{
            CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, GWLP_USERDATA,
            GetClientRect, GetWindowLongPtrW, HTMAXBUTTON, HTTRANSPARENT, HWND_TOP, IDC_ARROW,
            IsIconic, IsWindow, IsWindowVisible, IsZoomed, LoadCursorW, PostMessageW,
            RegisterClassExW, SC_MAXIMIZE, SC_RESTORE, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
            SetCursor, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_CANCELMODE,
            WM_CAPTURECHANGED, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE,
            WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
            WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_PAINT, WM_SETCURSOR, WM_SYSCOMMAND, WNDCLASSEXW,
            WS_CHILD, WS_CLIPSIBLINGS,
        },
    },
};

#[derive(Deserialize)]
pub struct SnapRect {
    pub visible: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Serialize)]
struct SnapVisual {
    hover: bool,
    active: bool,
}

struct SnapState {
    app: tauri::AppHandle,
    parent: HWND,
    hovered: bool,
    pressed: bool,
    last_emitted: (bool, bool),
}

static OVERLAYS: OnceLock<Mutex<HashMap<isize, isize>>> = OnceLock::new();
static SNAP_CLASS: OnceLock<Vec<u16>> = OnceLock::new();
static IS_WIN11: OnceLock<bool> = OnceLock::new();

fn overlays() -> &'static Mutex<HashMap<isize, isize>> {
    OVERLAYS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_windows_11() -> bool {
    *IS_WIN11.get_or_init(|| unsafe {
        type RtlGetVersionFn = unsafe extern "system" fn(*mut OSVERSIONINFOW) -> i32;
        let module_name: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
        let ntdll = GetModuleHandleW(module_name.as_ptr());
        if ntdll.is_null() {
            return false;
        }
        let Some(proc_fn) = GetProcAddress(ntdll, c"RtlGetVersion".as_ptr().cast()) else {
            return false;
        };
        let rtl_get_version: RtlGetVersionFn = std::mem::transmute(proc_fn);
        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..OSVERSIONINFOW::default()
        };
        rtl_get_version(&mut info) == 0 && info.dwMajorVersion >= 10 && info.dwBuildNumber >= 22000
    })
}

fn snap_class_name() -> &'static [u16] {
    SNAP_CLASS.get_or_init(|| {
        let name: Vec<u16> = "FesenSnapTarget\0".encode_utf16().collect();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(snap_proc),
            hInstance: unsafe { GetModuleHandleW(ptr::null()) },
            hCursor: unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) },
            hbrBackground: unsafe { GetStockObject(NULL_BRUSH) },
            lpszClassName: name.as_ptr(),
            ..WNDCLASSEXW::default()
        };
        unsafe { RegisterClassExW(&wc) };
        name
    })
}

fn emit_state(state: &mut SnapState) {
    let next = (state.hovered, state.pressed);
    if next != state.last_emitted {
        state.last_emitted = next;
        let _ = state.app.emit_to(
            "main",
            "win-max-state",
            SnapVisual {
                hover: state.hovered,
                active: state.pressed,
            },
        );
    }
}

fn point_from_lparam(lparam: LPARAM) -> POINT {
    let raw = lparam as u32;
    POINT {
        x: i32::from((raw & 0xffff) as i16),
        y: i32::from(((raw >> 16) & 0xffff) as i16),
    }
}

unsafe fn state_mut<'a>(hwnd: HWND) -> Option<&'a mut SnapState> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SnapState;
    unsafe { ptr.as_mut() }
}

unsafe fn arm_nonclient_leave(hwnd: HWND) {
    let mut tme = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE | TME_NONCLIENT,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    unsafe { TrackMouseEvent(&mut tme) };
}

unsafe extern "system" fn snap_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = unsafe { &*(lparam as *const CREATESTRUCTW) };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_NCDESTROY => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SnapState;
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            if !ptr.is_null() {
                let state = unsafe { Box::from_raw(ptr) };
                if let Ok(mut map) = overlays().lock()
                    && map.get(&(state.parent as isize)).copied() == Some(hwnd as isize)
                {
                    map.remove(&(state.parent as isize));
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_NCHITTEST => {
            if let Some(state) = unsafe { state_mut(hwnd) }
                && unsafe { IsWindowEnabled(state.parent) } == 0
            {
                return HTTRANSPARENT as LRESULT;
            }
            HTMAXBUTTON as LRESULT
        }
        WM_SETCURSOR => {
            unsafe { SetCursor(LoadCursorW(ptr::null_mut(), IDC_ARROW)) };
            1
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(hwnd, &mut ps);
                EndPaint(hwnd, &ps);
            }
            0
        }
        WM_NCMOUSEMOVE => {
            if let Some(state) = unsafe { state_mut(hwnd) }
                && !state.hovered
            {
                unsafe { arm_nonclient_leave(hwnd) };
                state.hovered = true;
                emit_state(state);
            }
            0
        }
        WM_NCMOUSELEAVE => {
            if let Some(state) = unsafe { state_mut(hwnd) }
                && state.hovered
            {
                state.hovered = false;
                if !state.pressed {
                    emit_state(state);
                }
            }
            0
        }
        WM_NCLBUTTONDOWN if wparam == HTMAXBUTTON as usize => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                state.pressed = true;
                unsafe { SetCapture(hwnd) };
                emit_state(state);
            }
            0
        }
        WM_NCLBUTTONDBLCLK => 0,
        WM_MOUSEMOVE => {
            if let Some(state) = unsafe { state_mut(hwnd) }
                && state.pressed
            {
                let mut rc = RECT::default();
                unsafe { GetClientRect(hwnd, &mut rc) };
                let inside = unsafe { PtInRect(&rc, point_from_lparam(lparam)) } != 0;
                if state.hovered != inside {
                    state.hovered = inside;
                    emit_state(state);
                }
                return 0;
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_LBUTTONUP => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let was_pressed = state.pressed;
                state.pressed = false;
                unsafe { ReleaseCapture() };
                let mut rc = RECT::default();
                unsafe { GetClientRect(hwnd, &mut rc) };
                let inside = unsafe { PtInRect(&rc, point_from_lparam(lparam)) } != 0;
                state.hovered = inside;
                emit_state(state);
                if inside {
                    unsafe { arm_nonclient_leave(hwnd) };
                }
                if was_pressed && inside {
                    let cmd = if unsafe { IsZoomed(state.parent) } != 0 {
                        SC_RESTORE
                    } else {
                        SC_MAXIMIZE
                    };
                    unsafe {
                        PostMessageW(state.parent, WM_SYSCOMMAND, cmd as WPARAM, 0);
                    }
                }
                return 0;
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_NCLBUTTONUP if wparam == HTMAXBUTTON as usize => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let was_pressed = state.pressed;
                state.pressed = false;
                emit_state(state);
                if was_pressed {
                    let cmd = if unsafe { IsZoomed(state.parent) } != 0 {
                        SC_RESTORE
                    } else {
                        SC_MAXIMIZE
                    };
                    unsafe {
                        PostMessageW(state.parent, WM_SYSCOMMAND, cmd as WPARAM, 0);
                    }
                }
            }
            0
        }
        WM_CAPTURECHANGED | WM_CANCELMODE => {
            if let Some(state) = unsafe { state_mut(hwnd) } {
                let clear_hover = msg == WM_CANCELMODE || !state.pressed;
                state.pressed = false;
                if clear_hover {
                    state.hovered = false;
                }
                emit_state(state);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub fn sync_snap_button(window: &WebviewWindow, rect: SnapRect) -> Result<(), String> {
    if !is_windows_11() {
        return Ok(());
    }
    let parent_raw = window.hwnd().map_err(|_| "invalid_window")?.0 as isize;
    let app = window.app_handle().clone();
    window
        .run_on_main_thread(move || unsafe {
            let parent = parent_raw as HWND;
            if IsWindow(parent) == 0 {
                return;
            }
            let existing = overlays()
                .lock()
                .ok()
                .and_then(|m| m.get(&parent_raw).copied())
                .filter(|&raw| IsWindow(raw as HWND) != 0);

            let can_show = rect.visible
                && rect.width > 0
                && rect.height > 0
                && IsIconic(parent) == 0
                && IsWindowVisible(parent) != 0;

            if !can_show {
                if let Some(child_raw) = existing {
                    let child = child_raw as HWND;
                    if let Some(state) = state_mut(child) {
                        state.hovered = false;
                        state.pressed = false;
                        emit_state(state);
                    }
                    ShowWindow(child, SW_HIDE);
                }
                return;
            }

            let child = match existing {
                Some(child_raw) => child_raw as HWND,
                None => {
                    let state = Box::new(SnapState {
                        app,
                        parent,
                        hovered: false,
                        pressed: false,
                        last_emitted: (false, false),
                    });
                    let ptr = Box::into_raw(state);
                    let created = CreateWindowExW(
                        0,
                        snap_class_name().as_ptr(),
                        ptr::null(),
                        WS_CHILD | WS_CLIPSIBLINGS,
                        0,
                        0,
                        0,
                        0,
                        parent,
                        ptr::null_mut(),
                        GetModuleHandleW(ptr::null()),
                        ptr.cast(),
                    );
                    if created.is_null() {
                        drop(Box::from_raw(ptr));
                        return;
                    }
                    if let Ok(mut map) = overlays().lock() {
                        map.insert(parent_raw, created as isize);
                    }
                    created
                }
            };

            let w = rect.width.clamp(1, 4096);
            let h = rect.height.clamp(1, 4096);
            let top_inset = if IsZoomed(parent) != 0 {
                0
            } else {
                3.min(h - 1)
            };
            SetWindowPos(
                child,
                HWND_TOP,
                rect.x,
                rect.y + top_inset,
                w,
                (h - top_inset).max(1),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        })
        .map_err(|_| "main_thread_unavailable".to_owned())
}
