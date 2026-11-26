import os
os.chdir(r"C:\work\wwd")


def patch(p, pairs):
    s = open(p, encoding="utf-8").read()
    for a, b in pairs:
        if a not in s:
            raise SystemExit(f"{p}: anchor not found: {a[:90]}")
        s = s.replace(a, b, 1)
    open(p, "w", encoding="utf-8").write(s)


patch("crates/wwd-linux/src/thunk/astc.rs", [("""pub fn note_cmd(cmd: u64, device: u64) {""", """pub fn device_phys(device: u64) -> Option<u64> {
    with(|s| s.devices.get(&device).copied())
}

pub fn note_cmd(cmd: u64, device: u64) {""")])
patch("crates/wwd-linux/src/thunk/mod.rs", [("pub mod astc;", "pub mod astc;\npub mod capture;")])
patch("crates/wwd-linux/src/thunk/vulkan.rs", [
    ("use super::{astc, guest_args, win64_call};", "use super::{astc, capture, guest_args, win64_call};"),
    ("""            // ---- ASTC emulation (see astc.rs) ----""", """            // ---- frame capture (debug, WWD_CAPTURE) ----
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
            // ---- ASTC emulation (see astc.rs) ----"""),
])
print("patched")
