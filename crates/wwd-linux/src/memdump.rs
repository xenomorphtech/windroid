//! Guest memory snapshot (WWD_MEMDUMP=<present frame>[,<frame>...]).
//!
//! Writes every committed read-write guest region to memdump/f<frame>/mem.bin
//! (regions back to back) and a /proc/pid/maps-style index, memdump/f<frame>/maps,
//! whose "offset" column is the region's position in mem.bin. Offline tools
//! that read /proc/<pid>/mem can then run against the snapshot (rebuild a
//! sparse file with each region at its address). Regions are labelled
//! "[anon:.bss]": wwd doesn't track mapping names.

use crate::mem::committed_rw_ranges;
use std::io::Write;

fn read(addr: u64, len: usize) -> Option<Vec<u8>> {
    use windows_sys::Win32::System::{Diagnostics::Debug::ReadProcessMemory, Threading::GetCurrentProcess};
    let mut buf = vec![0u8; len];
    let mut got = 0usize;
    let ok = unsafe { ReadProcessMemory(GetCurrentProcess(), addr as *const _, buf.as_mut_ptr() as *mut _, len, &mut got) };
    (ok != 0 && got == len).then_some(buf)
}

pub fn run(frame: u64) {
    let t0 = std::time::Instant::now();
    let dir = format!("memdump/f{frame}");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[wwd] memdump: {dir}: {e}");
        return;
    }
    let (Ok(mem), Ok(maps)) = (std::fs::File::create(format!("{dir}/mem.bin")), std::fs::File::create(format!("{dir}/maps"))) else {
        eprintln!("[wwd] memdump: can't create files in {dir}");
        return;
    };
    let (mut mem, mut maps) = (std::io::BufWriter::new(mem), std::io::BufWriter::new(maps));
    let mut off = 0u64;
    let mut n = 0;
    for (s, e) in committed_rw_ranges() {
        // split into 1 MiB pieces so a page decommitted mid-read only drops that piece
        let mut a = s;
        while a < e {
            let len = (e - a).min(1 << 20);
            if let Some(b) = read(a, len as usize) {
                let _ = mem.write_all(&b);
                let _ = writeln!(maps, "{:x}-{:x} rw-p {:08x} 00:00 0 [anon:.bss]", a, a + len, off);
                off += len;
                n += 1;
            }
            a += len;
        }
    }
    let _ = (mem.flush(), maps.flush());
    eprintln!("[wwd] memdump: {} MB in {n} regions to {dir} ({} ms)", off >> 20, t0.elapsed().as_millis());
}

/// WWD_RDOC=<frame>: ask an injected RenderDoc (renderdoccmd capture ...) to
/// capture the next frame, via its in-application API (TriggerCapture).
pub fn renderdoc_trigger(frame: u64) {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
    unsafe {
        let m = GetModuleHandleA(b"renderdoc.dll\0".as_ptr());
        if m.is_null() {
            eprintln!("[wwd] renderdoc: not injected (run under renderdoccmd capture)");
            return;
        }
        let Some(get_api) = GetProcAddress(m, b"RENDERDOC_GetAPI\0".as_ptr()) else { return };
        let get_api: unsafe extern "C" fn(u32, *mut *const usize) -> i32 = std::mem::transmute(get_api);
        let mut api: *const usize = std::ptr::null();
        if get_api(10102, &mut api) != 1 || api.is_null() {
            eprintln!("[wwd] renderdoc: GetAPI failed");
            return;
        }
        // RENDERDOC_API_1_1_2: [15] TriggerCapture()
        let trigger: unsafe extern "C" fn() = std::mem::transmute(*api.add(15));
        trigger();
        eprintln!("[wwd] renderdoc: capture triggered at frame {frame}");
    }
}
