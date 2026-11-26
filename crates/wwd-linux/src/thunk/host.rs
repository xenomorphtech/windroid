//! `svc #0x5758`: host services for the guest Android runtime (droid/).
//! x8 = service id, args in x0..x5, result in x0. Ids must match
//! droid/include/wwd_host.h.

use super::window;
use crate::mem::write_obj;
use crate::Thread;

pub const SVC_HOST: u64 = 0x5758;

pub fn dispatch(t: &mut Thread) {
    let id = t.cpu.x[8];
    let a = [t.cpu.x[0], t.cpu.x[1], t.cpu.x[2], t.cpu.x[3]];
    let r: u64 = unsafe {
        match id {
            // window_create(w, h) -> id
            1 => window::create(a[0] as u32, a[1] as u32),
            // window_size(id, *w, *h)
            2 => {
                let (w, h) = window::size(a[0]);
                write_obj(a[1], w);
                write_obj(a[2], h);
                0
            }
            // window_poll(id, *event) -> 1 if an event was written
            3 => match window::poll(a[0]) {
                Some(e) => {
                    write_obj(a[1], e);
                    1
                }
                None => 0,
            },
            // window_hwnd(id)
            4 => window::hwnd(a[0]),
            // log(level, tag, msg): forward Android log lines to stderr
            5 => {
                let tag = String::from_utf8_lossy(&crate::mem::cstr(a[1])).into_owned();
                let msg = String::from_utf8_lossy(&crate::mem::cstr(a[2])).into_owned();
                let lv = ["?", "?", "V", "D", "I", "W", "E", "F", "S"].get(a[0] as usize).copied().unwrap_or("?");
                eprintln!("{lv}/{tag}: {}", msg.trim_end());
                0
            }
            // monotonic_ns()
            6 => crate::util::monotonic_ns(),
            _ => {
                eprintln!("[wwd] unknown host service {id}");
                u64::MAX
            }
        }
    };
    t.cpu.x[0] = r;
}
