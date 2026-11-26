import os
os.chdir(r"C:\work\wwd")
here = os.path.join("scripts", "patches")


def patch(p, pairs, append=None):
    s = open(p, encoding="utf-8").read()
    for a, b in pairs:
        if a not in s:
            raise SystemExit(f"{p}: anchor not found: {a[:90]}")
        s = s.replace(a, b, 1)
    if append:
        s += append
    open(p, "w", encoding="utf-8").write(s)


patch("crates/wwd-linux/src/thunk/astc.rs", [("""    /// staging buffers we created: image -> [(device, buffer, memory)]
    staging: HashMap<u64, Vec<(u64, u64, u64)>>,
}""", """    /// staging buffers we created: image -> [(device, buffer, memory)]
    staging: HashMap<u64, Vec<(u64, u64, u64)>>,
    /// command buffer -> device
    cmds: HashMap<u64, u64>,
}

pub fn note_cmd(cmd: u64, device: u64) {
    with(|s| s.cmds.insert(cmd, device));
}
pub fn cmd_device(cmd: u64) -> Option<u64> {
    with(|s| s.cmds.get(&cmd).copied())
}""")])
patch("crates/wwd-linux/src/thunk/mod.rs", [("pub mod host;", "pub mod astc;\npub mod host;")])
hooks = open(os.path.join(here, "astc_hooks.rs.txt"), encoding="utf-8").read()
fns = open(os.path.join(here, "astc_fns.rs.txt"), encoding="utf-8").read()
patch("crates/wwd-linux/src/thunk/vulkan.rs", [
    ("""            "vkCreateAndroidSurfaceKHR" => create_android_surface(&args),""",
     hooks + """            "vkCreateAndroidSurfaceKHR" => create_android_surface(&args),"""),
    ("use super::{guest_args, win64_call};", "use super::{astc, guest_args, win64_call};"),
], append=fns)
print("patched")
