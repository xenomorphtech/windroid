//! Android system properties, served as bionic's "pre-split" property area.
//!
//! When /dev/__properties__ is a regular file, bionic maps it as a single
//! `prop_area` (ContextsPreSplit). Layout (bionic/libc/system_properties):
//!
//!   prop_area   { u32 bytes_used, u32 serial, u32 magic, u32 version, u32 reserved[28] } (128 B)
//!   data[]      bump-allocated, 4-byte aligned objects, offsets relative to data:
//!     prop_trie_node { u32 namelen, u32 prop, u32 left, u32 right, u32 children, char name[] }
//!     prop_info      { u32 serial, char value[92], char name[] }
//!
//! Names are split at '.'; each level is a binary search tree ordered by
//! (length, then bytes), and `children` links to the next level's root.

const PA_SIZE: usize = 128 * 1024;
const MAGIC: u32 = 0x504f_5250;
const VERSION: u32 = 0xfc6e_d0ab;
const HDR: usize = 128;
const PROP_VALUE_MAX: usize = 92;

struct Area {
    buf: Vec<u8>,
    used: usize,
}

impl Area {
    fn u32_at(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.buf[HDR + off..HDR + off + 4].try_into().unwrap())
    }
    fn set_u32(&mut self, off: usize, v: u32) {
        self.buf[HDR + off..HDR + off + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn alloc(&mut self, n: usize) -> usize {
        let off = self.used;
        self.used = (self.used + n + 3) & !3;
        assert!(HDR + self.used <= PA_SIZE, "property area full");
        off
    }
    fn new_node(&mut self, name: &[u8]) -> usize {
        let off = self.alloc(20 + name.len() + 1);
        self.set_u32(off, name.len() as u32);
        self.buf[HDR + off + 20..HDR + off + 20 + name.len()].copy_from_slice(name);
        off
    }
    fn name_of(&self, node: usize) -> &[u8] {
        let n = self.u32_at(node) as usize;
        &self.buf[HDR + node + 20..HDR + node + 20 + n]
    }
    fn cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
        a.len().cmp(&b.len()).then_with(|| a.cmp(b))
    }

    /// Find or insert `seg` in the BST rooted at the `children` field of `parent`.
    fn child(&mut self, parent: usize, seg: &[u8]) -> usize {
        let mut link = parent + 16; // &parent.children
        loop {
            let cur = self.u32_at(link) as usize;
            if cur == 0 {
                let n = self.new_node(seg);
                self.set_u32(link, n as u32);
                return n;
            }
            match Self::cmp(seg, self.name_of(cur)) {
                std::cmp::Ordering::Equal => return cur,
                std::cmp::Ordering::Less => link = cur + 8,
                std::cmp::Ordering::Greater => link = cur + 12,
            }
        }
    }

    fn add(&mut self, name: &str, value: &str) {
        let mut node = 0usize; // root
        for seg in name.split('.') {
            node = self.child(node, seg.as_bytes());
        }
        let v = value.as_bytes();
        let v = &v[..v.len().min(PROP_VALUE_MAX - 1)];
        let pi = self.alloc(4 + PROP_VALUE_MAX + name.len() + 1);
        self.set_u32(pi, (v.len() as u32) << 24);
        self.buf[HDR + pi + 4..HDR + pi + 4 + v.len()].copy_from_slice(v);
        self.buf[HDR + pi + 4 + PROP_VALUE_MAX..HDR + pi + 4 + PROP_VALUE_MAX + name.len()].copy_from_slice(name.as_bytes());
        self.set_u32(node + 4, pi as u32);
    }
}

/// Build the property area from (name, value) pairs (later entries win).
pub fn build(props: &[(String, String)]) -> Vec<u8> {
    let mut a = Area { buf: vec![0u8; PA_SIZE], used: 0 };
    a.new_node(b""); // root
    let mut seen = std::collections::BTreeMap::new();
    for (k, v) in props {
        seen.insert(k.clone(), v.clone());
    }
    for (k, v) in &seen {
        a.add(k, v);
    }
    let used = a.used as u32;
    a.buf[0..4].copy_from_slice(&used.to_le_bytes());
    a.buf[8..12].copy_from_slice(&MAGIC.to_le_bytes());
    a.buf[12..16].copy_from_slice(&VERSION.to_le_bytes());
    a.buf
}

/// Properties from the guest's build.prop files plus emulator-friendly defaults.
pub fn default_props(root: &std::path::Path) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for f in ["system/build.prop", "vendor/build.prop", "product/etc/build.prop"] {
        if let Ok(s) = std::fs::read_to_string(root.join(f)) {
            for line in s.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.starts_with("import") {
                    continue;
                }
                if let Some((k, val)) = line.split_once('=') {
                    v.push((k.trim().to_string(), val.trim().to_string()));
                }
            }
        }
    }
    for (k, val) in [
        ("ro.product.cpu.abi", "arm64-v8a"),
        ("ro.product.cpu.abilist", "arm64-v8a"),
        ("ro.product.cpu.abilist64", "arm64-v8a"),
        ("ro.product.manufacturer", "wwd"),
        ("ro.product.model", "wwd Windows"),
        ("ro.product.brand", "wwd"),
        ("ro.product.device", "wwd"),
        ("ro.hardware", "wwd"),
        ("ro.board.platform", "wwd"),
        ("ro.debuggable", "0"),
        ("ro.secure", "1"),
        ("ro.kernel.qemu", "0"),
        ("ro.boot.qemu", "0"),
        ("ro.hardware.egl", "wwd"),
        ("ro.hardware.vulkan", "wwd"),
        ("ro.opengles.version", "196610"), // GLES 3.2
        ("ro.sf.lcd_density", "320"),
        ("persist.sys.locale", "ko-KR"),
        ("ro.product.locale", "ko-KR"),
        ("sys.boot_completed", "1"),
        ("libc.debug.malloc.options", ""),
        ("heapprofd.enable", "0"),
    ] {
        v.push((k.to_string(), val.to_string()));
    }
    v
}

#[cfg(test)]
mod tests {
    #[test]
    fn builds() {
        let a = super::build(&[("ro.build.version.sdk".into(), "34".into()), ("ro.a".into(), "x".into())]);
        assert_eq!(&a[8..12], &0x504f_5250u32.to_le_bytes());
        assert!(a.windows(2).any(|w| w == b"34"));
    }
}
