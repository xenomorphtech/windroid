//! Vulkan: forward guest calls to the host loader (vulkan-1.dll).
//!
//! Translation is needed only where the platform shows through:
//!   * WSI: guest surface extensions (android/xlib/xcb/wayland) are swapped
//!     for VK_KHR_win32_surface when creating the instance.
//!   * callbacks: guest function pointers can't be called from host code yet,
//!     so pAllocator is dropped and debug messengers are stubbed out.

use super::vk_table::VK_CMDS;
use super::{astc, capture, guest_args, win64_call};
use crate::mem::{read_obj, write_obj};
use crate::Thread;
use std::ffi::CString;
use std::sync::Mutex;

struct Host {
    gipa: u64,
    instance: u64,
    cache: Vec<u64>,
    /// fake handles for stubbed debug messengers
    next_fake: u64,
}

static HOST: Mutex<Option<Host>> = Mutex::new(None);

const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;
const VK_ERROR_EXTENSION_NOT_PRESENT: i32 = -7;
const STYPE_DEBUG_REPORT_CI: u32 = 1000011000;
const STYPE_DEBUG_UTILS_MESSENGER_CI: u32 = 1000128004;

fn host() -> std::sync::MutexGuard<'static, Option<Host>> {
    let mut g = HOST.lock().unwrap();
    if g.is_none() {
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
        unsafe {
            let h = LoadLibraryA(b"vulkan-1.dll\0".as_ptr());
            let gipa = if h.is_null() { 0 } else { GetProcAddress(h, b"vkGetInstanceProcAddr\0".as_ptr()).map(|f| f as u64).unwrap_or(0) };
            if gipa == 0 {
                eprintln!("[wwd] vulkan: host vulkan-1.dll not available");
            }
            *g = Some(Host { gipa, instance: 0, cache: vec![0; VK_CMDS.len()], next_fake: 0x7777_0000 });
        }
    }
    g
}

fn gipa(h: &Host, instance: u64, name: &str) -> u64 {
    if h.gipa == 0 {
        return 0;
    }
    let c = CString::new(name).unwrap();
    win64_call(h.gipa, &[instance, c.as_ptr() as u64]).0
}

fn resolve(id: usize) -> u64 {
    let mut g = host();
    let h = g.as_mut().unwrap();
    if h.cache[id] == 0 {
        let inst = h.instance;
        h.cache[id] = gipa(h, inst, VK_CMDS[id].name);
        if h.cache[id] == 0 && inst != 0 {
            h.cache[id] = gipa(h, 0, VK_CMDS[id].name);
        }
    }
    h.cache[id]
}

unsafe fn cstr(p: u64) -> String {
    String::from_utf8_lossy(&crate::mem::cstr(p)).into_owned()
}

static LAST_VB: std::sync::Mutex<std::collections::BTreeMap<u64, (u64, u64)>> = std::sync::Mutex::new(std::collections::BTreeMap::new());
static LAST_IB: std::sync::Mutex<std::collections::BTreeMap<u64, (u64, u64, u64)>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

fn trace(t: &Thread, name: &str, args: &[u64], ret: u64) {
    if t.proc.cfg.strace {
        eprintln!("[{}] vk {}({}) = {:#x}", t.tid, name, args.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>().join(", "), ret as i64);
    }
}

pub fn call(t: &mut Thread, id: usize) -> (u64, u64) {
    if id == 0 {
        // has(handle, name, kind)
        let name = unsafe { cstr(t.cpu.x[1]) };
        // entry points implemented here rather than by the host driver
        if matches!(name.as_str(), "vkCreateAndroidSurfaceKHR" | "vkGetAndroidHardwareBufferPropertiesANDROID" | "vkGetMemoryAndroidHardwareBufferANDROID" | "vkCreateDebugUtilsMessengerEXT" | "vkDestroyDebugUtilsMessengerEXT" | "vkCreateDebugReportCallbackEXT" | "vkDestroyDebugReportCallbackEXT" | "vkSubmitDebugUtilsMessageEXT") {
            return (1, 0);
        }
        let g = host();
        let h = g.as_ref().unwrap();
        let handle = t.cpu.x[0];
        let f = if t.cpu.x[2] == 1 {
            let gdpa = gipa(h, h.instance, "vkGetDeviceProcAddr");
            let c = CString::new(name.clone()).unwrap();
            if gdpa == 0 { 0 } else { win64_call(gdpa, &[handle, c.as_ptr() as u64]).0 }
        } else {
            gipa(h, if handle != 0 { handle } else { h.instance }, &name)
        };
        if std::env::var_os("WWD_VK_TRACE").is_some() && (f == 0 || t.cpu.x[2] == 2) {
            let why = if t.cpu.x[2] == 2 { "NOT IN GUEST TABLE" } else { "host has no" };
            eprintln!("[wwd] vulkan: {why} {name} (kind {}, host {})", t.cpu.x[2], f != 0);
        }
        return ((f != 0) as u64, 0);
    }
    let Some(cmd) = VK_CMDS.get(id) else {
        eprintln!("[wwd] vulkan: bad thunk id {id}");
        return (VK_ERROR_INITIALIZATION_FAILED as u32 as u64, 0);
    };
    let mut args = guest_args(t, cmd.nparams as usize, cmd.float_mask);
    for (k, a) in args.iter_mut().enumerate() {
        if cmd.alloc_mask & (1 << k) != 0 {
            *a = 0; // host can't call guest allocation callbacks
        }
    }
    if std::env::var("WWD_VK_TRACE").map(|v| v == "2").unwrap_or(false) {
        eprintln!("[{}] vk> {}({})", t.tid, cmd.name, args.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>().join(", "));
    }
    let r = unsafe {
        match cmd.name {
            "vkCreateInstance" => create_instance(&args),
            "vkCreateDebugUtilsMessengerEXT" | "vkCreateDebugReportCallbackEXT" => {
                let mut g = host();
                let h = g.as_mut().unwrap();
                h.next_fake += 1;
                write_obj(args[3], h.next_fake);
                (0, 0)
            }
            "vkDestroyDebugUtilsMessengerEXT" | "vkDestroyDebugReportCallbackEXT" | "vkSubmitDebugUtilsMessageEXT" => (0, 0),
            // ---- draw dump (debug, WWD_DUMP_DRAW=<indexCount>) ----
            "vkCmdBindVertexBuffers" | "vkCmdBindVertexBuffers2" | "vkCmdBindVertexBuffers2EXT" if std::env::var_os("WWD_DUMP_DRAW").is_some() => {
                if args[2] > 0 {
                    let b: u64 = read_obj(args[3]);
                    let o: u64 = read_obj(args[4]);
                    LAST_VB.lock().unwrap().insert(args[0], (b, o));
                }
                win64_call(resolve(id), &args)
            }
            "vkCmdBindIndexBuffer" if std::env::var_os("WWD_DUMP_DRAW").is_some() => {
                LAST_IB.lock().unwrap().insert(args[0], (args[1], args[2], args[3]));
                win64_call(resolve(id), &args)
            }
            "vkCmdDrawIndexed" if std::env::var("WWD_DUMP_DRAW").ok().and_then(|v| v.parse::<u64>().ok()) == Some(args[1]) => {
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 300 == 5 {
                    let vb = LAST_VB.lock().unwrap().get(&args[0]).copied();
                    let ib = LAST_IB.lock().unwrap().get(&args[0]).copied();
                    eprintln!("[wwd] draw dump: count {} first {} vofs {} vb {vb:x?} ib {ib:x?}", args[1], args[3], args[4] as i32);
                    if let Some((ib_buf, ib_off, ity)) = ib {
                        if let Some(p) = astc::buffer_ptr(ib_buf) {
                            let p = p + ib_off + args[3] * if ity == 1 { 4 } else { 2 };
                            let idx: Vec<u32> = (0..args[1].min(54)).map(|i| if ity == 1 { read_obj::<u32>(p + i * 4) } else { read_obj::<u16>(p + i * 2) as u32 }).collect();
                            eprintln!("[wwd]   indices {idx:?}");
                        } else {
                            eprintln!("[wwd]   index buffer not mapped");
                        }
                    }
                    if let Some((b, o)) = vb {
                        if let Some(p) = astc::buffer_ptr(b) {
                            for v in 0..8u64 {
                                let base = p + o + (args[4] as i32 as i64 * 44) as u64 + v * 44;
                                let f: Vec<f32> = (0..8).map(|i| read_obj::<f32>(base + i * 4)).collect();
                                let c: [u8; 8] = read_obj(base + 32);
                                eprintln!("[wwd]   v{v}: tc {:?} mtc {:?} pos {:?} color {:02x?} px {:x}", &f[0..4], &f[4..6], &f[6..8], c, read_obj::<u32>(base + 40));
                            }
                        } else {
                            eprintln!("[wwd]   vertex buffer not mapped");
                        }
                    }
                }
                win64_call(resolve(id), &args)
            }
            // ---- frame capture (debug, WWD_CAPTURE) ----
            "vkCreateSwapchainKHR" => {
                capture::pre_create_swapchain(args[1]);
                let r = win64_call(resolve(id), &args);
                if r.0 as i32 == 0 {
                    capture::post_create_swapchain(args[0], args[1], read_obj(args[3]));
                }
                r
            }
            "vkGetSwapchainImagesKHR" => {
                let r = win64_call(resolve(id), &args);
                if r.0 as i32 >= 0 {
                    capture::note_images(args[1], args[2], args[3]);
                }
                r
            }
            "vkGetDeviceQueue" => {
                let r = win64_call(resolve(id), &args);
                capture::note_queue(args[0], args[1] as u32, read_obj(args[3]));
                r
            }
            "vkQueuePresentKHR" if capture::enabled() => {
                if capture::maybe_capture(vk_by_name, args[0], args[1]) {
                    // the capture submit consumed the wait semaphores
                    let n: u32 = astc::patch_u32(args[1] + 16, 0);
                    let r = win64_call(resolve(id), &args);
                    write_obj(args[1] + 16, n);
                    r
                } else {
                    win64_call(resolve(id), &args)
                }
            }
            // ---- ASTC emulation (see astc.rs) ----
            "vkGetPhysicalDeviceFeatures" => {
                let r = win64_call(resolve(id), &args);
                write_obj(args[1] + 84, 1u32); // textureCompressionASTC_LDR
                r
            }
            "vkGetPhysicalDeviceFeatures2" | "vkGetPhysicalDeviceFeatures2KHR" => {
                let r = win64_call(resolve(id), &args);
                write_obj(args[1] + 16 + 84, 1u32);
                r
            }
            "vkGetPhysicalDeviceFormatProperties" | "vkGetPhysicalDeviceFormatProperties2" | "vkGetPhysicalDeviceFormatProperties2KHR" => {
                let mut a = args.clone();
                let is_astc = astc::astc_info(a[1] as u32).is_some();
                a[1] = astc::replacement(a[1] as u32) as u64;
                let r = win64_call(resolve(id), &a);
                if is_astc {
                    // like real ASTC hardware: optimal tiling only, no buffer views
                    let props = if cmd.name == "vkGetPhysicalDeviceFormatProperties" { args[2] } else { args[2] + 16 };
                    write_obj(props, 0u32);
                    write_obj(props + 8, 0u32);
                }
                r
            }
            "vkGetPhysicalDeviceImageFormatProperties" if astc::astc_info(args[1] as u32).is_some() && args[3] == 1 => (-11i32 as u32 as u64, 0),
            "vkGetPhysicalDeviceImageFormatProperties" => {
                let mut a = args.clone();
                if astc::astc_info(a[1] as u32).is_some() {
                    a[1] = astc::replacement(a[1] as u32) as u64;
                    a[5] &= !0x80; // BLOCK_TEXEL_VIEW_COMPATIBLE
                }
                win64_call(resolve(id), &a)
            }
            "vkGetPhysicalDeviceImageFormatProperties2" | "vkGetPhysicalDeviceImageFormatProperties2KHR" => {
                let fmt: u32 = read_obj(args[1] + 16);
                if astc::astc_info(fmt).is_some() && read_obj::<u32>(args[1] + 24) == 1 {
                    (-11i32 as u32 as u64, 0) // no linear-tiled ASTC
                } else if astc::astc_info(fmt).is_some() {
                    let old = astc::patch_u32(args[1] + 16, astc::replacement(fmt));
                    let r = win64_call(resolve(id), &args);
                    write_obj(args[1] + 16, old);
                    r
                } else {
                    win64_call(resolve(id), &args)
                }
            }
            "vkCreateDevice" => {
                // the host has no ASTC: don't ask it for the feature we emulate
                let mut restore: Vec<(u64, u32)> = Vec::new();
                let feats: u64 = read_obj(args[1] + 64);
                if feats != 0 {
                    restore.push((feats + 84, astc::patch_u32(feats + 84, 0)));
                }
                let mut node: u64 = read_obj(args[1] + 8);
                while node != 0 {
                    if read_obj::<u32>(node) == 1000059000 {
                        restore.push((node + 16 + 84, astc::patch_u32(node + 16 + 84, 0)));
                    }
                    node = read_obj(node + 8);
                }
                let r = win64_call(resolve(id), &args);
                for (a, v) in restore {
                    write_obj(a, v);
                }
                if r.0 as i32 == 0 {
                    astc::note_device(read_obj(args[3]), args[0]);
                }
                r
            }
            "vkAllocateCommandBuffers" => {
                let r = win64_call(resolve(id), &args);
                if r.0 as i32 == 0 {
                    let n: u32 = read_obj(args[1] + 28);
                    for i in 0..n as u64 {
                        astc::note_cmd(read_obj(args[2] + i * 8), args[0]);
                    }
                }
                r
            }
            "vkCreateImage" if trace_images() && {
                // VkImageCreateInfo: format @24, extent @28, mips @40, layers @44, samples @48, usage @56
                let ci = args[1];
                let usage: u32 = read_obj(ci + 56);
                if usage & 0x30 != 0 {
                    // color or depth/stencil attachment
                    eprintln!(
                        "[wwd] image fmt {} {}x{}x{} mips {} layers {} samples {} usage {:#x}",
                        read_obj::<u32>(ci + 24), read_obj::<u32>(ci + 28), read_obj::<u32>(ci + 32), read_obj::<u32>(ci + 36),
                        read_obj::<u32>(ci + 40), read_obj::<u32>(ci + 44), read_obj::<u32>(ci + 48), usage
                    );
                }
                false
            } => unreachable!(),
            "vkCreateImage" => {
                let fmt: u32 = read_obj(args[1] + 24);
                if astc::astc_info(fmt).is_some() {
                    let old_fmt = astc::patch_u32(args[1] + 24, astc::replacement(fmt));
                    let flags: u32 = read_obj(args[1] + 16);
                    write_obj(args[1] + 16, flags & !0x80);
                    let list = patch_view_formats(args[1]);
                    let r = win64_call(resolve(id), &args);
                    restore_u32s(list);
                    write_obj(args[1] + 24, old_fmt);
                    write_obj(args[1] + 16, flags);
                    if r.0 as i32 == 0 {
                        astc::note_image(read_obj(args[3]), fmt);
                    } else {
                        let info: [u32; 22] = read_obj(args[1]);
                        let mut chain = Vec::new();
                        let mut node: u64 = read_obj(args[1] + 8);
                        while node != 0 && chain.len() < 8 {
                            chain.push(read_obj::<u32>(node));
                            node = read_obj(node + 8);
                        }
                        eprintln!("[wwd] astc: vkCreateImage({fmt}) failed {}: flags {:#x} type {} extent {}x{}x{} mips {} layers {} samples {} tiling {} usage {:#x} pNext {:?}",
                            r.0 as i32, info[4], info[5], info[7], info[8], info[9], info[10], info[11], info[12], info[13], info[14], chain);
                    }
                    r
                } else {
                    win64_call(resolve(id), &args)
                }
            }
            "vkGetDeviceImageMemoryRequirements" | "vkGetDeviceImageMemoryRequirementsKHR" | "vkGetDeviceImageSparseMemoryRequirements" | "vkGetDeviceImageSparseMemoryRequirementsKHR" => {
                // VkDeviceImageMemoryRequirements.pCreateInfo (+16) -> VkImageCreateInfo
                let ci: u64 = read_obj(args[1] + 16);
                let fmt: u32 = read_obj(ci + 24);
                if astc::astc_info(fmt).is_some() {
                    let old_fmt = astc::patch_u32(ci + 24, astc::replacement(fmt));
                    let flags: u32 = read_obj(ci + 16);
                    write_obj(ci + 16, flags & !0x80);
                    let list = patch_view_formats(ci);
                    let r = win64_call(resolve(id), &args);
                    restore_u32s(list);
                    write_obj(ci + 24, old_fmt);
                    write_obj(ci + 16, flags);
                    r
                } else {
                    win64_call(resolve(id), &args)
                }
            }
            "vkCreateImageView" => {
                let fmt: u32 = read_obj(args[1] + 36);
                if astc::astc_info(fmt).is_some() {
                    let old = astc::patch_u32(args[1] + 36, astc::replacement(fmt));
                    let r = win64_call(resolve(id), &args);
                    write_obj(args[1] + 36, old);
                    r
                } else {
                    win64_call(resolve(id), &args)
                }
            }
            "vkDestroyImage" => {
                let r = win64_call(resolve(id), &args);
                astc::free_staging(vk_by_name, astc::forget_image(args[1]));
                r
            }
            "vkBindBufferMemory" => {
                astc::note_buffer_bind(args[1], args[2], args[3]);
                win64_call(resolve(id), &args)
            }
            "vkBindBufferMemory2" | "vkBindBufferMemory2KHR" => {
                for i in 0..args[1] {
                    let info = args[2] + i * 40;
                    astc::note_buffer_bind(read_obj(info + 16), read_obj(info + 24), read_obj(info + 32));
                }
                win64_call(resolve(id), &args)
            }
            "vkMapMemory" => {
                let r = win64_call(resolve(id), &args);
                if r.0 as i32 == 0 {
                    astc::note_map(args[1], args[2], read_obj(args[5]));
                }
                r
            }
            "vkMapMemory2" | "vkMapMemory2KHR" => {
                let r = win64_call(resolve(id), &args);
                if r.0 as i32 == 0 {
                    astc::note_map(read_obj(args[1] + 24), read_obj(args[1] + 32), read_obj(args[2]));
                }
                r
            }
            "vkUnmapMemory" => {
                astc::note_unmap(args[1]);
                win64_call(resolve(id), &args)
            }
            "vkCmdCopyBufferToImage" => match astc::image_format(args[2]) {
                Some(fmt) => copy_astc_v1(id, &args, fmt),
                None => win64_call(resolve(id), &args),
            },
            "vkCmdCopyBufferToImage2" | "vkCmdCopyBufferToImage2KHR" => match astc::image_format(read_obj(args[1] + 24)) {
                Some(fmt) => copy_astc_v2(id, &args, fmt),
                None => win64_call(resolve(id), &args),
            },
            "vkCreateAndroidSurfaceKHR" => create_android_surface(&args),
            // no AHardwareBuffers exist under wwd
            "vkGetAndroidHardwareBufferPropertiesANDROID" | "vkGetMemoryAndroidHardwareBufferANDROID" => (-1000072003i32 as u32 as u64, 0),
            "vkEnumerateInstanceExtensionProperties" => {
                let f = resolve(id);
                let r = win64_call(f, &args);
                // present the host's win32 WSI as Android's
                if r.0 as i32 >= 0 && args[2] != 0 {
                    let n: u32 = read_obj(args[1]);
                    for i in 0..n as u64 {
                        let e = args[2] + i * 260;
                        if crate::mem::cstr(e) == b"VK_KHR_win32_surface" {
                            let name = b"VK_KHR_android_surface ";
                            std::ptr::copy_nonoverlapping(name.as_ptr(), e as *mut u8, name.len());
                            write_obj(e + 256, 6u32);
                        }
                    }
                }
                r
            }
            _ => {
                let f = resolve(id);
                if f == 0 {
                    eprintln!("[wwd] vulkan: {} not available on host", cmd.name);
                    (VK_ERROR_EXTENSION_NOT_PRESENT as u32 as u64, 0)
                } else {
                    win64_call(f, &args)
                }
            }
        }
    };
    trace(t, cmd.name, &args, r.0);
    report_error(cmd.name, &args, r.0);
    r
}

/// WWD_TRACE_IMAGES=1: log render-target creations (format, size, usage).
fn trace_images() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WWD_TRACE_IMAGES").is_some())
}

/// Log failing VkResult-returning calls (first few per command and code): a
/// failed pipeline or image creation makes UE silently skip those draws.
fn report_error(name: &str, args: &[u64], ret: u64) {
    let code = ret as u32 as i32;
    if code >= 0 {
        return;
    }
    let returns_result = ["vkCreate", "vkAllocate", "vkBind", "vkMap", "vkQueueSubmit", "vkQueuePresent", "vkBeginCommandBuffer", "vkEndCommandBuffer", "vkAcquire", "vkWaitFor"]
        .iter()
        .any(|p| name.starts_with(p));
    if !returns_result {
        return;
    }
    static SEEN: std::sync::Mutex<Option<std::collections::HashMap<(String, i32), u32>>> = std::sync::Mutex::new(None);
    let mut g = SEEN.lock().unwrap();
    let n = g.get_or_insert_with(Default::default).entry((name.to_string(), code)).or_insert(0);
    *n += 1;
    if *n <= 5 || n.is_power_of_two() {
        eprintln!(
            "[wwd] vulkan: {name} failed: {code} (#{n}) args {}",
            args.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>().join(", ")
        );
    }
}

/// vkCreateInstance with WSI extension translation and debug-callback stripping.
unsafe fn create_instance(args: &[u64]) -> (u64, u64) {
    let (ci, _alloc, p_instance) = (args[0], args[1], args[2]);
    // shallow copy of VkInstanceCreateInfo (64 bytes)
    let mut info: [u64; 8] = read_obj(ci);
    let n_ext: u32 = read_obj(ci + 48);
    let exts: u64 = read_obj(ci + 56);
    let mut names: Vec<CString> = Vec::new();
    let mut want_win32 = false;
    for i in 0..n_ext as u64 {
        let s = cstr(read_obj(exts + i * 8));
        match s.as_str() {
            "VK_KHR_xlib_surface" | "VK_KHR_xcb_surface" | "VK_KHR_wayland_surface" | "VK_KHR_android_surface" | "VK_EXT_headless_surface" => want_win32 = true,
            _ => names.push(CString::new(s).unwrap()),
        }
    }
    if want_win32 && !names.iter().any(|n| n.as_bytes() == b"VK_KHR_win32_surface") {
        names.push(CString::new("VK_KHR_win32_surface").unwrap());
    }
    let ptrs: Vec<u64> = names.iter().map(|c| c.as_ptr() as u64).collect();
    info[6] = ptrs.len() as u64; // enabledExtensionCount (u32 at +48, upper half padding)
    info[7] = ptrs.as_ptr() as u64;

    // unlink debug-callback create infos from the pNext chain (restored after)
    let mut patched: Vec<(u64, u64)> = Vec::new();
    let mut prev = info.as_mut_ptr() as u64; // our copy's pNext lives at +8
    let mut node: u64 = info[1];
    while node != 0 {
        let stype: u32 = read_obj(node);
        let next: u64 = read_obj(node + 8);
        if stype == STYPE_DEBUG_UTILS_MESSENGER_CI || stype == STYPE_DEBUG_REPORT_CI {
            patched.push((prev + 8, read_obj(prev + 8)));
            write_obj(prev + 8, next);
        } else {
            prev = node;
        }
        node = next;
    }

    let f = resolve(VK_CMDS.iter().position(|c| c.name == "vkCreateInstance").unwrap());
    let r = win64_call(f, &[info.as_ptr() as u64, 0, p_instance]);
    for (addr, old) in patched.into_iter().rev() {
        write_obj(addr, old);
    }
    if r.0 as u32 == 0 {
        let inst: u64 = read_obj(p_instance);
        let mut g = host();
        let h = g.as_mut().unwrap();
        h.instance = inst;
        h.cache.iter_mut().for_each(|c| *c = 0);
    }
    r
}

/// VkAndroidSurfaceCreateInfoKHR.window is our guest ANativeWindow
/// (droid/include/wwd_host.h: { u32 magic; u32 refs; u64 host_window; ... }).
unsafe fn create_android_surface(args: &[u64]) -> (u64, u64) {
    let (instance, ci, _alloc, p_surface) = (args[0], args[1], args[2], args[3]);
    let win: u64 = read_obj(ci + 24);
    let magic: u32 = read_obj(crate::mem::untag(win));
    if magic != 0x5744_5757 {
        eprintln!("[wwd] vulkan: vkCreateAndroidSurfaceKHR with foreign ANativeWindow {win:#x}");
        return (VK_ERROR_INITIALIZATION_FAILED as u32 as u64, 0);
    }
    let host_id: u64 = read_obj(crate::mem::untag(win) + 8);
    let hwnd = super::window::hwnd(host_id);
    // VkWin32SurfaceCreateInfoKHR
    let info: [u64; 5] = [1000009000, 0, 0, super::window::hinstance(), hwnd];
    let f = {
        let g = host();
        let h = g.as_ref().unwrap();
        gipa(h, instance, "vkCreateWin32SurfaceKHR")
    };
    if f == 0 {
        return (VK_ERROR_EXTENSION_NOT_PRESENT as u32 as u64, 0);
    }
    win64_call(f, &[instance, info.as_ptr() as u64, 0, p_surface])
}

/// Host entry point by name (device-level names resolve through the instance).
fn vk_by_name(name: &str) -> u64 {
    match VK_CMDS.iter().position(|c| c.name == name) {
        Some(id) => resolve(id),
        None => 0,
    }
}

/// Map ASTC entries of a VkImageFormatListCreateInfo (sType 1000147000) in a
/// VkImageCreateInfo's pNext chain to their RGBA8 replacements, so the list
/// matches the emulated image and its views. Returns what to restore.
unsafe fn patch_view_formats(ci: u64) -> Vec<(u64, u32)> {
    let mut old = Vec::new();
    let mut node: u64 = read_obj(ci + 8);
    for _ in 0..16 {
        if node == 0 {
            break;
        }
        if read_obj::<u32>(node) == 1000147000 {
            let (n, list): (u32, u64) = (read_obj(node + 16), read_obj(node + 24));
            for i in 0..n.min(64) as u64 {
                let f: u32 = read_obj(list + 4 * i);
                if astc::astc_info(f).is_some() {
                    old.push((list + 4 * i, astc::patch_u32(list + 4 * i, astc::replacement(f))));
                }
            }
        }
        node = read_obj(node + 8);
    }
    old
}

unsafe fn restore_u32s(v: Vec<(u64, u32)>) {
    for (a, x) in v {
        write_obj(a, x);
    }
}

/// vkCmdCopyBufferToImage into an emulated ASTC image (VkBufferImageCopy, 56 bytes).
unsafe fn copy_astc_v1(id: usize, args: &[u64], fmt: u32) -> (u64, u64) {
    let (cmd, src, dst, layout, n, regs) = (args[0], args[1], args[2], args[3], args[4] as u32, args[5]);
    let Some(device) = astc::cmd_device(cmd) else { return win64_call(resolve(id), args) };
    let mut rs = Vec::new();
    for i in 0..n as u64 {
        let r = regs + i * 56;
        rs.push(astc::Region {
            buffer_offset: read_obj(r),
            row_length: read_obj(r + 8),
            image_height: read_obj(r + 12),
            layers: read_obj(r + 28),
            width: read_obj(r + 44),
            height: read_obj(r + 48),
            depth: read_obj(r + 52),
        });
    }
    let Some((staging, offs)) = astc::decode_upload(vk_by_name, device, src, dst, fmt, &rs) else {
        eprintln!("[wwd] astc: upload decode failed; skipping copy");
        return (0, 0);
    };
    let mut copy = vec![0u8; 56 * n as usize];
    std::ptr::copy_nonoverlapping(crate::mem::untag(regs) as *const u8, copy.as_mut_ptr(), copy.len());
    for (i, off) in offs.iter().enumerate() {
        let b = &mut copy[i * 56..];
        b[0..8].copy_from_slice(&off.to_le_bytes());
        b[8..16].fill(0); // rowLength, imageHeight = tightly packed
    }
    win64_call(resolve(id), &[cmd, staging, dst, layout, n as u64, copy.as_ptr() as u64])
}

/// vkCmdCopyBufferToImage2 (VkCopyBufferToImageInfo2 + VkBufferImageCopy2, 72 bytes).
unsafe fn copy_astc_v2(id: usize, args: &[u64], fmt: u32) -> (u64, u64) {
    let (cmd, info) = (args[0], args[1]);
    let Some(device) = astc::cmd_device(cmd) else { return win64_call(resolve(id), args) };
    let (src, dst): (u64, u64) = (read_obj(info + 16), read_obj(info + 24));
    let n: u32 = read_obj(info + 36);
    let regs: u64 = read_obj(info + 40);
    let mut rs = Vec::new();
    for i in 0..n as u64 {
        let r = regs + i * 72;
        rs.push(astc::Region {
            buffer_offset: read_obj(r + 16),
            row_length: read_obj(r + 24),
            image_height: read_obj(r + 28),
            layers: read_obj(r + 44),
            width: read_obj(r + 60),
            height: read_obj(r + 64),
            depth: read_obj(r + 68),
        });
    }
    let Some((staging, offs)) = astc::decode_upload(vk_by_name, device, src, dst, fmt, &rs) else {
        eprintln!("[wwd] astc: upload decode failed; skipping copy");
        return (0, 0);
    };
    let mut copy = vec![0u8; 72 * n as usize];
    std::ptr::copy_nonoverlapping(crate::mem::untag(regs) as *const u8, copy.as_mut_ptr(), copy.len());
    for (i, off) in offs.iter().enumerate() {
        let b = &mut copy[i * 72..];
        b[8..16].fill(0); // pNext
        b[16..24].copy_from_slice(&off.to_le_bytes());
        b[24..32].fill(0);
    }
    let mut ci = [0u8; 48];
    std::ptr::copy_nonoverlapping(crate::mem::untag(info) as *const u8, ci.as_mut_ptr(), 48);
    ci[8..16].fill(0);
    ci[16..24].copy_from_slice(&staging.to_le_bytes());
    ci[40..48].copy_from_slice(&(copy.as_ptr() as u64).to_le_bytes());
    win64_call(resolve(id), &[cmd, ci.as_ptr() as u64])
}
