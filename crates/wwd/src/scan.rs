//! `wwd --scan <elf>`: report instruction encodings the CPU can't execute yet.

use yaxpeax_arch::{Decoder, U8Reader};
use yaxpeax_arm::armv8::a64::InstDecoder;

pub fn disasm(w: u32) -> String {
    let bytes = w.to_le_bytes();
    let mut r = U8Reader::new(&bytes);
    match InstDecoder::default().decode(&mut r) {
        Ok(i) => format!("{i}"),
        Err(_) => "<invalid>".into(),
    }
}

pub fn run(path: &str) {
    let data = std::fs::read(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1)
    });
    let u16le = |o: usize| u16::from_le_bytes(data[o..o + 2].try_into().unwrap());
    let u32le = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let u64le = |o: usize| u64::from_le_bytes(data[o..o + 8].try_into().unwrap());
    let phoff = u64le(32) as usize;
    let phentsize = u16le(54) as usize;
    let mut total = 0usize;
    // Prefer section headers (SHF_EXECINSTR) so .rodata sharing the RX
    // segment doesn't show up as bogus instructions; fall back to PF_X segments.
    let shoff = u64le(40) as usize;
    let shentsize = u16le(58) as usize;
    let shnum = u16le(60) as usize;
    let mut ranges: Vec<(usize, u64, usize)> = Vec::new();
    for i in 0..shnum {
        let o = shoff + i * shentsize;
        if o + 64 > data.len() {
            break;
        }
        let flags = u64le(o + 8);
        if u32le(o + 4) == 1 && flags & 4 != 0 {
            ranges.push((u64le(o + 24) as usize, u64le(o + 16), u64le(o + 32) as usize));
        }
    }
    if ranges.is_empty() {
        for i in 0..u16le(56) as usize {
            let o = phoff + i * phentsize;
            if u32le(o) == 1 && u32le(o + 4) & 1 != 0 {
                ranges.push((u64le(o + 8) as usize, u64le(o + 16), u64le(o + 32) as usize));
            }
        }
    }
    let _ = phentsize;
    for (off, vaddr, filesz) in ranges {
        let code = &data[off..off + filesz];
        println!("executable segment {vaddr:#x}..{:#x} ({} words)", vaddr + filesz as u64, filesz / 4);
        for (key, n, w, pc) in wwd_cpu::coverage(code, vaddr) {
            let d = disasm(w);
            if d == "<invalid>" {
                continue; // data in the text section (literal pools, jump tables)
            }
            total += n;
            println!("  {n:6}x  {key:08x}  e.g. {w:08x} @ {pc:#x}  {d}");
        }
    }
    println!("{total} executable-looking words not covered");
}

/// `wwd --disasm FILE OFFSET COUNT`: disassemble COUNT words at file offset.
pub fn disasm_file(path: &str, off: &str, count: &str) {
    let data = std::fs::read(path).expect("read");
    let off = u64::from_str_radix(off.trim_start_matches("0x"), 16).expect("hex offset") as usize;
    let n: usize = count.parse().unwrap_or(32);
    for i in 0..n {
        let o = off + i * 4;
        if o + 4 > data.len() {
            break;
        }
        let w = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let mut extra = String::new();
        // resolve PC-relative branch targets
        if w & 0xfc00_0000 == 0x9400_0000 || w & 0xfc00_0000 == 0x1400_0000 {
            let imm = (((w & 0x03ff_ffff) as i64) << 38) >> 36;
            extra = format!("   -> {:#x}", o as i64 + imm);
        }
        println!("{o:#10x}: {w:08x}  {}{extra}", disasm(w));
    }
}
