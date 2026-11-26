//! ELF64 AArch64 loader (static, static-PIE and dynamically linked binaries).

use crate::mem::{self, align_down, align_up, PAGE};

const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_AARCH64: u16 = 183;

#[derive(Clone, Copy, Debug)]
pub struct Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
}

#[derive(Debug)]
pub struct Image {
    /// Load bias (0 for ET_EXEC).
    pub bias: u64,
    pub entry: u64,
    /// Address of the program headers in guest memory.
    pub phdr: u64,
    pub phnum: u64,
    pub end: u64,
    pub interp: Option<String>,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

pub fn load(data: &[u8]) -> Result<Image, String> {
    if data.len() < 64 || &data[0..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    if data[4] != 2 || data[5] != 1 {
        return Err("not a little-endian ELF64 file".into());
    }
    let e_type = u16le(data, 16);
    let machine = u16le(data, 18);
    if machine != EM_AARCH64 {
        return Err(format!("unsupported machine {machine} (want AArch64 = 183)"));
    }
    if e_type != ET_EXEC && e_type != ET_DYN {
        return Err(format!("unsupported ELF type {e_type}"));
    }
    let entry = u64le(data, 24);
    let phoff = u64le(data, 32) as usize;
    let phentsize = u16le(data, 54) as usize;
    let phnum = u16le(data, 56) as usize;

    let mut phdrs = Vec::new();
    for i in 0..phnum {
        let o = phoff + i * phentsize;
        if o + 56 > data.len() {
            return Err("truncated program headers".into());
        }
        phdrs.push(Phdr {
            p_type: u32le(data, o),
            p_flags: u32le(data, o + 4),
            p_offset: u64le(data, o + 8),
            p_vaddr: u64le(data, o + 16),
            p_filesz: u64le(data, o + 32),
            p_memsz: u64le(data, o + 40),
        });
    }
    let loads: Vec<&Phdr> = phdrs.iter().filter(|p| p.p_type == PT_LOAD).collect();
    if loads.is_empty() {
        return Err("no PT_LOAD segments".into());
    }
    let lo = align_down(loads.iter().map(|p| p.p_vaddr).min().unwrap(), PAGE);
    let hi = align_up(loads.iter().map(|p| p.p_vaddr + p.p_memsz).max().unwrap(), PAGE);
    let span = hi - lo;

    // Reserve the whole image span in one go so segments stay contiguous.
    let bias = if e_type == ET_EXEC {
        let r = mem::map_anon(lo, span, 0, true);
        if r < 0 {
            return Err(format!("cannot map image at {lo:#x} (address in use)"));
        }
        0
    } else {
        let r = mem::map_anon(0, span, 0, false);
        if r < 0 {
            return Err("out of address space for image".into());
        }
        (r as u64).wrapping_sub(lo)
    };

    for p in &loads {
        let start = align_down(p.p_vaddr, PAGE) + bias;
        let end = align_up(p.p_vaddr + p.p_memsz, PAGE) + bias;
        if mem::protect(start, end - start, mem::PROT_READ | mem::PROT_WRITE) < 0 {
            return Err(format!("cannot commit segment at {start:#x}"));
        }
        let off = p.p_offset as usize;
        let fsz = p.p_filesz as usize;
        if off + fsz > data.len() {
            return Err("segment extends past end of file".into());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(data[off..].as_ptr(), (p.p_vaddr + bias) as *mut u8, fsz);
        }
        // bss is already zero (fresh pages)
    }
    // Apply final protections. Segments may share a page; merge permissions
    // per page so a RW page is never downgraded by its RX neighbour.
    let mut pages: std::collections::BTreeMap<u64, u32> = Default::default();
    for p in &loads {
        let prot = ((p.p_flags & 4 != 0) as u32 * mem::PROT_READ) | ((p.p_flags & 2 != 0) as u32 * mem::PROT_WRITE) | ((p.p_flags & 1 != 0) as u32 * mem::PROT_EXEC);
        let mut a = align_down(p.p_vaddr, PAGE);
        while a < p.p_vaddr + p.p_memsz {
            *pages.entry(a + bias).or_insert(0) |= prot;
            a += PAGE;
        }
    }
    for (a, prot) in pages {
        mem::protect(a, PAGE, prot);
    }

    let interp = phdrs.iter().find(|p| p.p_type == PT_INTERP).map(|p| {
        let s = &data[p.p_offset as usize..(p.p_offset + p.p_filesz) as usize];
        String::from_utf8_lossy(s.split(|&b| b == 0).next().unwrap_or(&[])).into_owned()
    });

    let phdr = match phdrs.iter().find(|p| p.p_type == PT_PHDR) {
        Some(p) => p.p_vaddr + bias,
        None => {
            // find the PT_LOAD that contains the file offset of the headers
            let o = phoff as u64;
            loads
                .iter()
                .find(|p| o >= p.p_offset && o < p.p_offset + p.p_filesz)
                .map(|p| p.p_vaddr + (o - p.p_offset) + bias)
                .unwrap_or(0)
        }
    };

    Ok(Image { bias, entry: entry + bias, phdr, phnum: phnum as u64, end: hi + bias, interp })
}
