//! Win32 windows: the hidden UI window, one borderless output window per
//! monitor (spec 7) and the full-screen pairing PIN window.

use std::cell::RefCell;

use anyhow::{Result, anyhow};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::shared::WM_ANNEX_WAKE;

pub const WM_ANNEX_DISPLAY_CHANGED: u32 = WM_ANNEX_WAKE + 1;
pub const WM_ANNEX_CANCEL_PAIRING: u32 = WM_ANNEX_WAKE + 2;

thread_local! {
    static HIDDEN: RefCell<HWND> = const { RefCell::new(HWND(std::ptr::null_mut())) };
    static PIN_TEXT: RefCell<(String, String)> = const { RefCell::new((String::new(), String::new())) };
}

fn instance() -> windows::Win32::Foundation::HINSTANCE {
    unsafe { GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default() }
}

unsafe extern "system" fn hidden_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_DISPLAYCHANGE {
            let _ = PostMessageW(Some(hwnd), WM_ANNEX_DISPLAY_CHANGED, WPARAM(0), LPARAM(0));
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

unsafe extern "system" fn output_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SETCURSOR => {
                SetCursor(None);
                LRESULT(1)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe extern "system" fn pin_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                FillRect(hdc, &rc, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(0x00FF_FFFF));
                let (pin, sub) = PIN_TEXT.with(|t| t.borrow().clone());
                let h = rc.bottom - rc.top;
                let big = CreateFontW(h / 5, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, FF_DONTCARE.0 as u32, w!("Segoe UI"));
                let small = CreateFontW(h / 28, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, FF_DONTCARE.0 as u32, w!("Segoe UI"));
                let old = SelectObject(hdc, big.into());
                let mut top = rc;
                top.bottom = rc.top + h * 62 / 100;
                let mut pinw: Vec<u16> = pin.encode_utf16().collect();
                DrawTextW(hdc, &mut pinw, &mut top, DT_CENTER | DT_BOTTOM | DT_SINGLELINE);
                SelectObject(hdc, small.into());
                let mut bottom = rc;
                bottom.top = rc.top + h * 66 / 100;
                let mut subw: Vec<u16> = sub.encode_utf16().collect();
                DrawTextW(hdc, &mut subw, &mut bottom, DT_CENTER | DT_TOP | DT_WORDBREAK);
                SelectObject(hdc, old);
                let _ = DeleteObject(big.into());
                let _ = DeleteObject(small.into());
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
                cancel_pairing();
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn cancel_pairing() {
    HIDDEN.with(|h| unsafe {
        let _ = PostMessageW(Some(*h.borrow()), WM_ANNEX_CANCEL_PAIRING, WPARAM(0), LPARAM(0));
    });
}

pub fn register_classes() -> Result<()> {
    unsafe {
        let hi = instance();
        type Proc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
        let procs: [(PCWSTR, Proc, HBRUSH); 3] = [
            (w!("AnnexHidden"), hidden_proc, HBRUSH::default()),
            (w!("AnnexOutput"), output_proc, HBRUSH::default()),
            (w!("AnnexPin"), pin_proc, HBRUSH(GetStockObject(BLACK_BRUSH).0)),
        ];
        for (name, proc_, brush) in procs {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc_),
                hInstance: hi,
                lpszClassName: name,
                hbrBackground: brush,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            if RegisterClassExW(&wc) == 0 {
                return Err(anyhow!("RegisterClassExW failed"));
            }
        }
    }
    Ok(())
}

/// A never-shown top-level window: receives wake-ups, timers, the hotkey and
/// `WM_DISPLAYCHANGE` broadcasts.
pub fn create_hidden() -> Result<HWND> {
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("AnnexHidden"),
            w!("Annex"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance()),
            None,
        )?
    };
    HIDDEN.with(|h| *h.borrow_mut() = hwnd);
    Ok(hwnd)
}

/// Borderless, topmost, black window exactly covering a monitor. The swap
/// chain renders into it; there is no GDI redirection surface.
pub fn create_output(rect: RECT) -> Result<HWND> {
    unsafe {
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            w!("AnnexOutput"),
            w!("Annex display"),
            WS_POPUP,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(instance()),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        Ok(hwnd)
    }
}

pub fn destroy(hwnd: HWND) {
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// Full-screen black window with the pairing PIN.
pub fn show_pin(rect: RECT, pin: &str, sub: &str) -> Result<HWND> {
    set_pin_text(pin, sub);
    unsafe {
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("AnnexPin"),
            w!("Annex pairing"),
            WS_POPUP,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(instance()),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        Ok(hwnd)
    }
}

pub fn set_pin_text(pin: &str, sub: &str) {
    PIN_TEXT.with(|t| *t.borrow_mut() = (pin.to_string(), sub.to_string()));
}

pub fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

pub fn message_box(text: &str) {
    let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(None, PCWSTR(t.as_ptr()), w!("Annex"), MB_OK | MB_ICONINFORMATION);
    }
}
