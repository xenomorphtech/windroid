//! Debug frame capture: WWD_CAPTURE="300,900" dumps the swapchain image of those
//! presents to capture_<n>.bmp (current directory) straight from the GPU.

use super::win64_call;
use crate::mem::{read_obj, write_obj};
use std::collections::HashMap;
use std::sync::Mutex;

pub type Resolver = fn(&str) -> u64;

#[derive(Default)]
struct State {
    swapchains: HashMap<u64, (u64, u32, u32, u32)>, // swapchain -> (device, format, w, h)
    images: HashMap<u64, Vec<u64>>,
    queues: HashMap<u64, (u64, u32)>, // queue -> (device, family)
    frames: u64,
    wanted: Vec<u64>,
    font_fix: Vec<u64>,
    mem_dump: Vec<u64>,
    rdoc: Vec<u64>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap();
    f(g.get_or_insert_with(|| State {
        wanted: std::env::var("WWD_CAPTURE").map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default(),
        font_fix: std::env::var("WWD_FONT_FIX").map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default(),
        rdoc: std::env::var("WWD_RDOC").map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default(),
        mem_dump: std::env::var("WWD_MEMDUMP").map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect()).unwrap_or_default(),
        ..Default::default()
    }))
}

pub fn enabled() -> bool {
    with(|s| !s.wanted.is_empty() || !s.font_fix.is_empty() || !s.mem_dump.is_empty())
}

/// Before vkCreateSwapchainKHR: make images readable by transfers.
pub unsafe fn pre_create_swapchain(ci: u64) {
    if enabled() {
        let usage: u32 = read_obj(ci + 56);
        write_obj(ci + 56, usage | 1); // TRANSFER_SRC
    }
}

pub unsafe fn post_create_swapchain(device: u64, ci: u64, sc: u64) {
    let fmt: u32 = read_obj(ci + 36);
    let w: u32 = read_obj(ci + 44);
    let h: u32 = read_obj(ci + 48);
    with(|s| s.swapchains.insert(sc, (device, fmt, w, h)));
}

pub unsafe fn note_images(sc: u64, count_p: u64, images_p: u64) {
    if images_p == 0 {
        return;
    }
    let n: u32 = read_obj(count_p);
    let v: Vec<u64> = (0..n as u64).map(|i| read_obj(images_p + i * 8)).collect();
    with(|s| s.images.insert(sc, v));
}

pub fn note_queue(device: u64, family: u32, queue: u64) {
    with(|s| s.queues.insert(queue, (device, family)));
}

/// Called before vkQueuePresentKHR. Returns true if it consumed the present's
/// wait semaphores (the caller must then present without waiting).
pub unsafe fn maybe_capture(vk: Resolver, queue: u64, present: u64) -> bool {
    let (frame, want, fix, dump, rdoc) = with(|s| {
        s.frames += 1;
        (s.frames, s.wanted.contains(&s.frames), s.font_fix.contains(&s.frames), s.mem_dump.contains(&s.frames), s.rdoc.contains(&s.frames))
    });
    if rdoc {
        crate::memdump::renderdoc_trigger(frame);
    }
    fps_tick(frame);
    if dump {
        crate::memdump::run(frame);
    }
    if fix {
        eprintln!("[wwd] fontfix at frame {frame}");
        crate::fontfix::run();
    }
    if !want {
        return false;
    }
    let sc: u64 = read_obj(read_obj::<u64>(present + 40));
    let idx: u32 = read_obj(read_obj::<u64>(present + 48));
    let info = with(|s| (s.swapchains.get(&sc).copied(), s.images.get(&sc).and_then(|v| v.get(idx as usize).copied()), s.queues.get(&queue).copied()));
    let (Some((device, fmt, w, h)), Some(image), Some((_, family))) = info else {
        eprintln!("[wwd] capture: missing swapchain/queue info");
        return false;
    };
    let size = w as u64 * h as u64 * 4;

    // host-visible readback buffer
    let ci: [u64; 7] = [12, 0, 0, size, 2 /* TRANSFER_DST */, 0, 0];
    let mut buf = 0u64;
    win64_call(vk("vkCreateBuffer"), &[device, ci.as_ptr() as u64, 0, &mut buf as *mut u64 as u64]);
    let mut req = [0u64; 3];
    win64_call(vk("vkGetBufferMemoryRequirements"), &[device, buf, req.as_mut_ptr() as u64]);
    let phys = super::astc::device_phys(device).unwrap_or(0);
    let mut props = vec![0u8; 520];
    win64_call(vk("vkGetPhysicalDeviceMemoryProperties"), &[phys, props.as_mut_ptr() as u64]);
    let count = u32::from_le_bytes(props[0..4].try_into().unwrap());
    let ty = (0..count.min(32)).find(|&i| req[2] as u32 & (1 << i) != 0 && u32::from_le_bytes(props[4 + i as usize * 8..8 + i as usize * 8].try_into().unwrap()) & 6 == 6);
    let Some(ty) = ty else { return false };
    let ai: [u64; 4] = [5, 0, req[0], ty as u64];
    let mut mem = 0u64;
    win64_call(vk("vkAllocateMemory"), &[device, ai.as_ptr() as u64, 0, &mut mem as *mut u64 as u64]);
    win64_call(vk("vkBindBufferMemory"), &[device, buf, mem, 0]);

    // command buffer
    let pci: [u32; 6] = [39, 0, 0, 0, 2, family]; // sType, pad, pNext(2 words), flags, family
    let mut pool = 0u64;
    win64_call(vk("vkCreateCommandPool"), &[device, pci.as_ptr() as u64, 0, &mut pool as *mut u64 as u64]);
    let cai: [u64; 4] = [40, 0, pool, 1u64 << 32];
    let mut cmd = 0u64;
    win64_call(vk("vkAllocateCommandBuffers"), &[device, cai.as_ptr() as u64, &mut cmd as *mut u64 as u64]);
    let bi: [u64; 4] = [42, 0, 1, 0];
    win64_call(vk("vkBeginCommandBuffer"), &[cmd, bi.as_ptr() as u64]);
    let barrier = |old: u32, new: u32, src: u32, dst: u32| {
        let mut b = [0u8; 72];
        b[0..4].copy_from_slice(&45u32.to_le_bytes());
        b[16..20].copy_from_slice(&src.to_le_bytes());
        b[20..24].copy_from_slice(&dst.to_le_bytes());
        b[24..28].copy_from_slice(&old.to_le_bytes());
        b[28..32].copy_from_slice(&new.to_le_bytes());
        b[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        b[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
        b[40..48].copy_from_slice(&image.to_le_bytes());
        b[48..52].copy_from_slice(&1u32.to_le_bytes()); // COLOR
        b[56..60].copy_from_slice(&1u32.to_le_bytes());
        b[64..68].copy_from_slice(&1u32.to_le_bytes());
        b
    };
    const PRESENT: u32 = 1000001002;
    let b1 = barrier(PRESENT, 6, 0, 0x800);
    win64_call(vk("vkCmdPipelineBarrier"), &[cmd, 0x10000, 0x1000, 0, 0, 0, 0, 0, 1, b1.as_ptr() as u64]);
    let mut region = [0u8; 56];
    region[16..20].copy_from_slice(&1u32.to_le_bytes());
    region[28..32].copy_from_slice(&1u32.to_le_bytes());
    region[44..48].copy_from_slice(&w.to_le_bytes());
    region[48..52].copy_from_slice(&h.to_le_bytes());
    region[52..56].copy_from_slice(&1u32.to_le_bytes());
    win64_call(vk("vkCmdCopyImageToBuffer"), &[cmd, image, 6, buf, 1, region.as_ptr() as u64]);
    let b2 = barrier(6, PRESENT, 0x800, 0);
    win64_call(vk("vkCmdPipelineBarrier"), &[cmd, 0x1000, 0x2000, 0, 0, 0, 0, 0, 1, b2.as_ptr() as u64]);
    win64_call(vk("vkEndCommandBuffer"), &[cmd]);

    // submit, consuming the present's wait semaphores
    let nwait: u32 = read_obj(present + 16);
    let waits: u64 = read_obj(present + 24);
    let stages = vec![0x10000u32; nwait as usize];
    let mut si = [0u8; 72];
    si[0..4].copy_from_slice(&4u32.to_le_bytes());
    si[16..20].copy_from_slice(&nwait.to_le_bytes());
    si[24..32].copy_from_slice(&waits.to_le_bytes());
    si[32..40].copy_from_slice(&(stages.as_ptr() as u64).to_le_bytes());
    si[40..44].copy_from_slice(&1u32.to_le_bytes());
    si[48..56].copy_from_slice(&(&cmd as *const u64 as u64).to_le_bytes());
    win64_call(vk("vkQueueSubmit"), &[queue, 1, si.as_ptr() as u64, 0]);
    win64_call(vk("vkQueueWaitIdle"), &[queue]);

    let mut ptr = 0u64;
    win64_call(vk("vkMapMemory"), &[device, mem, 0, u64::MAX, 0, &mut ptr as *mut u64 as u64]);
    let px = std::slice::from_raw_parts(ptr as *const u8, size as usize);
    let bgra = matches!(fmt, 44 | 50); // B8G8R8A8_UNORM / _SRGB
    let path = format!("capture_{frame}.bmp");
    write_bmp(&path, w, h, px, bgra);
    let nonblack = px.chunks(4).filter(|p| p[0] > 8 || p[1] > 8 || p[2] > 8).count();
    eprintln!("[wwd] capture: frame {frame} {w}x{h} fmt {fmt} -> {path} ({:.1}% non-black pixels)", nonblack as f64 * 100.0 / (w * h) as f64);
    win64_call(vk("vkUnmapMemory"), &[device, mem]);
    win64_call(vk("vkFreeCommandBuffers"), &[device, pool, 1, &cmd as *const u64 as u64]);
    win64_call(vk("vkDestroyCommandPool"), &[device, pool, 0]);
    win64_call(vk("vkDestroyBuffer"), &[device, buf, 0]);
    win64_call(vk("vkFreeMemory"), &[device, mem, 0]);
    true
}

fn write_bmp(path: &str, w: u32, h: u32, px: &[u8], bgra: bool) {
    let row = w as usize * 4;
    let mut out = Vec::with_capacity(54 + px.len());
    let file_size = 54 + px.len() as u32;
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes()); // top-down
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let p = &px[y * row + x * 4..y * row + x * 4 + 4];
            if bgra {
                out.extend_from_slice(&[p[0], p[1], p[2], 255]);
            } else {
                out.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
    }
    let _ = std::fs::write(path, out);
}

/// WWD_FPS=1: print the present rate every 2 seconds.
fn fps_tick(frame: u64) {
    use std::sync::{Mutex, OnceLock};
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("WWD_FPS").is_some()) {
        return;
    }
    static LAST: Mutex<Option<(std::time::Instant, u64)>> = Mutex::new(None);
    let now = std::time::Instant::now();
    let mut l = LAST.lock().unwrap();
    match *l {
        None => *l = Some((now, frame)),
        Some((t, f)) => {
            let dt = now.duration_since(t).as_secs_f64();
            if dt >= 2.0 {
                eprintln!("[wwd] fps {:.1} (frame {frame})", (frame - f) as f64 / dt);
                *l = Some((now, frame));
            }
        }
    }
}
