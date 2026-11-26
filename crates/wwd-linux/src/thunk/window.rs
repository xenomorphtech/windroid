//! Host windows for guest ANativeWindows.
//!
//! All windows live on one host UI thread that runs the Win32 message loop.
//! The window procedure turns messages into simple events that the guest
//! (our libandroid) polls and converts into AInputEvents.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Event record shared with the guest (must match droid/include/wwd_host.h).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Event {
    /// 1 = pointer down, 2 = pointer up, 3 = pointer move, 4 = key down,
    /// 5 = key up, 6 = resize, 7 = close, 8 = focus gained, 9 = focus lost
    pub kind: u32,
    pub key: u32,
    pub x: f32,
    pub y: f32,
    pub w: u32,
    pub h: u32,
}

struct Win {
    hwnd: u64,
    w: u32,
    h: u32,
    events: VecDeque<Event>,
}

static WINDOWS: Mutex<Vec<Win>> = Mutex::new(Vec::new());
static UI: OnceLock<mpsc::Sender<(u32, u32, mpsc::Sender<u64>)>> = OnceLock::new();

fn push(hwnd: HWND, e: Event) {
    let mut ws = WINDOWS.lock().unwrap();
    if let Some(w) = ws.iter_mut().find(|w| w.hwnd == hwnd as u64) {
        if e.kind == 6 {
            w.w = e.w;
            w.h = e.h;
        }
        w.events.push_back(e);
    }
}

fn lparam_xy(l: LPARAM) -> (f32, f32) {
    let x = (l & 0xffff) as i16 as f32;
    let y = ((l >> 16) & 0xffff) as i16 as f32;
    (x, y)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE => {
            let (x, y) = lparam_xy(lp);
            let kind = match msg {
                WM_LBUTTONDOWN => 1,
                WM_LBUTTONUP => 2,
                _ => {
                    if wp & 1 == 0 {
                        return 0; // only report drags (touch semantics)
                    }
                    3
                }
            };
            if msg == WM_LBUTTONDOWN {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::SetCapture(hwnd);
            } else if msg == WM_LBUTTONUP {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
            }
            push(hwnd, Event { kind, x, y, ..Default::default() });
            0
        }
        WM_KEYDOWN | WM_KEYUP => {
            push(hwnd, Event { kind: if msg == WM_KEYDOWN { 4 } else { 5 }, key: wp as u32, ..Default::default() });
            0
        }
        WM_SIZE => {
            let w = (lp & 0xffff) as u32;
            let h = ((lp >> 16) & 0xffff) as u32;
            if w > 0 && h > 0 {
                push(hwnd, Event { kind: 6, w, h, ..Default::default() });
            }
            0
        }
        WM_SETFOCUS => {
            push(hwnd, Event { kind: 8, ..Default::default() });
            0
        }
        WM_KILLFOCUS => {
            push(hwnd, Event { kind: 9, ..Default::default() });
            0
        }
        WM_CLOSE => {
            push(hwnd, Event { kind: 7, ..Default::default() });
            0 // the guest decides when to quit
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn ui_thread(rx: mpsc::Receiver<(u32, u32, mpsc::Sender<u64>)>) {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide("wwd_android_window");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: GetStockObject(BLACK_BRUSH) as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        RegisterClassW(&wc);
        loop {
            // create any requested windows, then pump messages briefly
            while let Ok((w, h, reply)) = rx.try_recv() {
                let mut r = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
                AdjustWindowRect(&mut r, WS_OVERLAPPEDWINDOW, 0);
                let title = wide("wwd — Android app");
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    title.as_ptr(),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    r.right - r.left,
                    r.bottom - r.top,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    hinst,
                    std::ptr::null(),
                );
                WINDOWS.lock().unwrap().push(Win { hwnd: hwnd as u64, w, h, events: VecDeque::new() });
                let _ = reply.send(hwnd as u64);
            }
            let mut msg: MSG = std::mem::zeroed();
            if MsgWaitForMultipleObjects(0, std::ptr::null(), 0, 10, QS_ALLINPUT) != WAIT_TIMEOUT {
                while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

/// Create a window of client size w x h; returns its handle id (index + 1).
pub fn create(w: u32, h: u32) -> u64 {
    let tx = UI.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new().name("wwd-ui".into()).spawn(move || ui_thread(rx)).unwrap();
        tx
    });
    let (rtx, rrx) = mpsc::channel();
    tx.send((w, h, rtx)).unwrap();
    let hwnd = rrx.recv().unwrap();
    let ws = WINDOWS.lock().unwrap();
    ws.iter().position(|x| x.hwnd == hwnd).map(|i| i as u64 + 1).unwrap_or(0)
}

pub fn hwnd(id: u64) -> u64 {
    WINDOWS.lock().unwrap().get((id as usize).wrapping_sub(1)).map(|w| w.hwnd).unwrap_or(0)
}

pub fn size(id: u64) -> (u32, u32) {
    WINDOWS.lock().unwrap().get((id as usize).wrapping_sub(1)).map(|w| (w.w, w.h)).unwrap_or((0, 0))
}

pub fn poll(id: u64) -> Option<Event> {
    WINDOWS.lock().unwrap().get_mut((id as usize).wrapping_sub(1)).and_then(|w| w.events.pop_front())
}

pub fn hinstance() -> u64 {
    unsafe { GetModuleHandleW(std::ptr::null()) as u64 }
}
