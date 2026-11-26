//! ASTC texture emulation for host GPUs without ASTC (desktop NVIDIA/AMD).
//!
//! Android games ship ASTC-compressed textures. We advertise ASTC support,
//! back ASTC images with RGBA8 (UNORM or SRGB), and decode block data on the
//! CPU when the app records a buffer->image copy into such an image: the
//! decoded texels go into a host-visible staging buffer we own, and the copy
//! is re-recorded from it.

use super::win64_call;
use crate::mem::{read_obj, write_obj};
use std::collections::HashMap;
use std::sync::Mutex;

pub const R8G8B8A8_UNORM: u32 = 37;
pub const R8G8B8A8_SRGB: u32 = 43;

/// (block width, block height, srgb) for VK_FORMAT_ASTC_*_BLOCK (157..=184).
pub fn astc_info(fmt: u32) -> Option<(u32, u32, bool)> {
    if !(157..=184).contains(&fmt) {
        return None;
    }
    const DIMS: [(u32, u32); 14] = [(4, 4), (5, 4), (5, 5), (6, 5), (6, 6), (8, 5), (8, 6), (8, 8), (10, 5), (10, 6), (10, 8), (10, 10), (12, 10), (12, 12)];
    let i = ((fmt - 157) / 2) as usize;
    Some((DIMS[i].0, DIMS[i].1, (fmt - 157) % 2 == 1))
}

pub fn replacement(fmt: u32) -> u32 {
    match astc_info(fmt) {
        Some((_, _, true)) => R8G8B8A8_SRGB,
        Some(_) => R8G8B8A8_UNORM,
        None => fmt,
    }
}

#[derive(Default)]
struct State {
    /// emulated images: image -> ASTC format
    images: HashMap<u64, u32>,
    /// buffer -> (memory, offset in memory)
    buffers: HashMap<u64, (u64, u64)>,
    /// memory -> host pointer of offset 0 (while mapped by the app)
    mapped: HashMap<u64, u64>,
    /// device -> physical device
    devices: HashMap<u64, u64>,
    /// staging buffers we created: image -> [(device, buffer, memory)]
    staging: HashMap<u64, Vec<(u64, u64, u64)>>,
    /// command buffer -> device
    cmds: HashMap<u64, u64>,
}

pub fn device_phys(device: u64) -> Option<u64> {
    with(|s| s.devices.get(&device).copied())
}

pub fn note_cmd(cmd: u64, device: u64) {
    with(|s| s.cmds.insert(cmd, device));
}
pub fn cmd_device(cmd: u64) -> Option<u64> {
    with(|s| s.cmds.get(&cmd).copied())
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap();
    f(g.get_or_insert_with(State::default))
}

pub fn note_device(device: u64, phys: u64) {
    with(|s| s.devices.insert(device, phys));
}
pub fn note_image(image: u64, astc_fmt: u32) {
    with(|s| s.images.insert(image, astc_fmt));
}
pub fn image_format(image: u64) -> Option<u32> {
    with(|s| s.images.get(&image).copied())
}
pub fn note_buffer_bind(buffer: u64, memory: u64, offset: u64) {
    with(|s| s.buffers.insert(buffer, (memory, offset)));
}
pub fn note_map(memory: u64, offset: u64, ptr: u64) {
    with(|s| s.mapped.insert(memory, ptr.wrapping_sub(offset)));
}
/// Host pointer of a buffer's start, if its memory is currently mapped.
pub fn buffer_ptr(buffer: u64) -> Option<u64> {
    with(|s| {
        let (m, off) = *s.buffers.get(&buffer)?;
        Some(s.mapped.get(&m)? + off)
    })
}
pub fn note_unmap(memory: u64) {
    with(|s| s.mapped.remove(&memory));
}

/// Forget an image; returns staging buffers to destroy.
pub fn forget_image(image: u64) -> Vec<(u64, u64, u64)> {
    with(|s| {
        s.images.remove(&image);
        s.staging.remove(&image).unwrap_or_default()
    })
}

/// Host function lookup supplied by vulkan.rs.
pub type Resolver = fn(&str) -> u64;

/// One buffer->image copy region in a common shape.
#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub buffer_offset: u64,
    pub row_length: u32,
    pub image_height: u32,
    pub layers: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

/// Decode `regions` of ASTC data from the app's source buffer into a new
/// staging buffer. Returns (staging buffer, per-region offsets in it).
pub unsafe fn decode_upload(vk: Resolver, device: u64, src_buffer: u64, image: u64, fmt: u32, regions: &[Region]) -> Option<(u64, Vec<u64>)> {
    let (bw, bh, _) = astc_info(fmt)?;
    let (memory, bind_off) = with(|s| s.buffers.get(&src_buffer).copied())?;
    let (phys, mapped) = with(|s| (s.devices.get(&device).copied(), s.mapped.get(&memory).copied()));
    let phys = phys?;

    // source pointer: the app's mapping, or map it ourselves for the duration
    let mut own_map = false;
    let base = match mapped {
        Some(p) => p,
        None => {
            let mut ptr = 0u64;
            let r = win64_call(vk("vkMapMemory"), &[device, memory, 0, u64::MAX, 0, &mut ptr as *mut u64 as u64]).0 as i32;
            if r != 0 {
                eprintln!("[wwd] astc: cannot map source memory ({r})");
                return None;
            }
            own_map = true;
            ptr
        }
    };

    // sizes of decoded output
    let mut offsets = Vec::with_capacity(regions.len());
    let mut total = 0u64;
    for rg in regions {
        offsets.push(total);
        total += rg.width as u64 * rg.height as u64 * rg.depth.max(1) as u64 * rg.layers.max(1) as u64 * 4;
        total = (total + 15) & !15;
    }
    let (staging, smem, sptr) = create_staging(vk, device, phys, total.max(16))?;

    for (rg, &dst_off) in regions.iter().zip(&offsets) {
        let row_len = if rg.row_length == 0 { rg.width } else { rg.row_length };
        let img_h = if rg.image_height == 0 { rg.height } else { rg.image_height };
        let blocks_x = row_len.div_ceil(bw) as u64;
        let blocks_y = img_h.div_ceil(bh) as u64;
        let slice = blocks_x * blocks_y * 16;
        let slices = rg.layers.max(1) as u64 * rg.depth.max(1) as u64;
        let src = base + bind_off + rg.buffer_offset;
        let out = (sptr + dst_off) as *mut u8;
        let (w, h) = (rg.width as usize, rg.height as usize);
        for sl in 0..slices {
            let sbase = src + sl * slice;
            let obase = out.add(sl as usize * w * h * 4);
            for by in 0..rg.height.div_ceil(bh) as u64 {
                for bx in 0..rg.width.div_ceil(bw) as u64 {
                    let block: [u8; 16] = read_obj(sbase + (by * blocks_x + bx) * 16);
                    astc_decode::astc_decode_block(&block, astc_decode::Footprint::new(bw, bh), |x, y, px| {
                        let gx = bx as usize * bw as usize + x as usize;
                        let gy = by as usize * bh as usize + y as usize;
                        if gx < w && gy < h {
                            std::ptr::copy_nonoverlapping(px.as_ptr(), obase.add((gy * w + gx) * 4), 4);
                        }
                    });
                }
            }
        }
    }
    if own_map {
        win64_call(vk("vkUnmapMemory"), &[device, memory]);
    }
    with(|s| s.staging.entry(image).or_default().push((device, staging, smem)));
    Some((staging, offsets))
}

/// A host-visible, coherent, persistently mapped TRANSFER_SRC buffer.
unsafe fn create_staging(vk: Resolver, device: u64, phys: u64, size: u64) -> Option<(u64, u64, u64)> {
    // VkBufferCreateInfo
    let mut ci = [0u64; 7];
    ci[0] = 12; // sType BUFFER_CREATE_INFO
    ci[3] = size;
    ci[4] = 1; // usage TRANSFER_SRC (u32), sharingMode EXCLUSIVE (0)
    let mut buf = 0u64;
    if win64_call(vk("vkCreateBuffer"), &[device, ci.as_ptr() as u64, 0, &mut buf as *mut u64 as u64]).0 as i32 != 0 {
        return None;
    }
    let mut req = [0u64; 3]; // size, alignment, memoryTypeBits
    win64_call(vk("vkGetBufferMemoryRequirements"), &[device, buf, req.as_mut_ptr() as u64]);
    let mut props = vec![0u8; 520];
    win64_call(vk("vkGetPhysicalDeviceMemoryProperties"), &[phys, props.as_mut_ptr() as u64]);
    let count = u32::from_le_bytes(props[0..4].try_into().unwrap());
    let bits = req[2] as u32;
    let mut ty = None;
    for i in 0..count.min(32) {
        let flags = u32::from_le_bytes(props[4 + i as usize * 8..8 + i as usize * 8].try_into().unwrap());
        if bits & (1 << i) != 0 && flags & 6 == 6 {
            ty = Some(i);
            break;
        }
    }
    let ty = ty?;
    let ai: [u64; 4] = [5, 0, req[0], ty as u64]; // MEMORY_ALLOCATE_INFO
    let mut mem = 0u64;
    if win64_call(vk("vkAllocateMemory"), &[device, ai.as_ptr() as u64, 0, &mut mem as *mut u64 as u64]).0 as i32 != 0 {
        win64_call(vk("vkDestroyBuffer"), &[device, buf, 0]);
        return None;
    }
    win64_call(vk("vkBindBufferMemory"), &[device, buf, mem, 0]);
    let mut ptr = 0u64;
    win64_call(vk("vkMapMemory"), &[device, mem, 0, u64::MAX, 0, &mut ptr as *mut u64 as u64]);
    Some((buf, mem, ptr))
}

/// Free staging resources (called on vkDestroyImage).
pub unsafe fn free_staging(vk: Resolver, items: Vec<(u64, u64, u64)>) {
    for (device, buf, mem) in items {
        win64_call(vk("vkDestroyBuffer"), &[device, buf, 0]);
        win64_call(vk("vkFreeMemory"), &[device, mem, 0]);
    }
}

// ---- struct field helpers -------------------------------------------------

pub unsafe fn patch_u32(addr: u64, v: u32) -> u32 {
    let old: u32 = read_obj(addr);
    write_obj(addr, v);
    old
}
