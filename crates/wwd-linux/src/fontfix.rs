//! Overgeared UI font repair (WWD_FONT_FIX=<present frame>[,<frame>...]).
//!
//! The shipped UI composite fonts (Default, Title, Number, NotoSansTC) use
//! NotoSansTC faces, which have no Hangul glyphs, while the UI text is Korean
//! (the defect the local-server project repairs with Frida on the Android
//! emulator by repointing entries to the NotoSansKR faces once those load).
//!
//! The faces are LazyLoad: Slate reads the .ufont from the (encrypted) pak
//! when it first needs a face, so their data can't be swapped in memory.
//! Instead every typeface entry of the UI composite fonts is turned into a
//! plain file-based FFontData pointing at the system's
//! /system/fonts/NotoSansCJK-Regular.ttc (sub-face 1 = Korean: Hangul + Han +
//! Latin), and the font's HistoryRevision is bumped so Slate rebuilds its
//! cached faces.
//!
//! Layout (UFont/FCompositeFont offsets verified for this build by the
//! local-server project):
//!   UFont+320 CompositeFont.DefaultTypeface.Fonts (TArray<FTypefaceEntry>)
//!   FCompositeFont: bEnableAscentDescentOverride at +56, HistoryRevision at +60
//!   FTypefaceEntry (48 bytes): FName Name @0, FFontData Font @8:
//!     FString FontFilename @8, Hinting u8 @24, LoadingPolicy u8 @25,
//!     SubFaceIndex i32 @28, FontFaceAsset @40 (entry-relative)

use crate::mem::committed_rw_ranges;
use std::collections::HashMap;

const FONT_PATH: &str = "/system/fonts/NotoSansCJK-Regular.ttc";
const KOREAN_SUBFACE: i32 = 1; // JP, KR, SC, TC, HK

/// Copy guest memory without faulting: other guest threads may decommit
/// pages (madvise) while we scan, so never touch it directly.
fn safe_read(addr: u64, len: usize) -> Option<Vec<u8>> {
    use windows_sys::Win32::System::{Diagnostics::Debug::ReadProcessMemory, Threading::GetCurrentProcess};
    let mut buf = vec![0u8; len];
    let mut got = 0usize;
    let ok = unsafe { ReadProcessMemory(GetCurrentProcess(), addr as *const _, buf.as_mut_ptr() as *mut _, len, &mut got) };
    (ok != 0 && got == len).then_some(buf)
}

fn safe_write<T: Copy>(addr: u64, v: T) {
    use windows_sys::Win32::System::{Diagnostics::Debug::WriteProcessMemory, Threading::GetCurrentProcess};
    unsafe { WriteProcessMemory(GetCurrentProcess(), addr as *const _, &v as *const T as *const _, std::mem::size_of::<T>(), std::ptr::null_mut()) };
}

fn rd<T: Copy>(b: &[u8], o: usize) -> T {
    unsafe { std::ptr::read_unaligned(b.as_ptr().add(o) as *const T) }
}

struct Font {
    array: u64,      // address of the TArray (UFont+320)
    entries: u64,    // entry data
    faces: Vec<u64>, // FontFaceAsset of each entry (0 = already converted)
}

/// Path buffers already handed to typeface entries (each must be owned once).
static USED: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

pub fn run() {
    let t0 = std::time::Instant::now();
    let ranges = committed_rw_ranges();
    let total: u64 = ranges.iter().map(|(s, e)| e - s).sum();
    let path_len = FONT_PATH.len() as i32 + 1;
    // 0. the guest's pool of Unreal-allocated path strings (droid java_impl.c)
    let needle: Vec<u8> = FONT_PATH.encode_utf16().chain(std::iter::once(0)).flat_map(|c| c.to_le_bytes()).collect();
    let mut pool = Vec::new();
    {
        const C: u64 = 1 << 20;
        for &(s, e) in &ranges {
            let mut a = s;
            while a < e {
                let n = (e - a).min(C);
                if let Some(b) = safe_read(a, n as usize) {
                    let mut o = 0;
                    while o + needle.len() <= b.len() {
                        if b[o..o + needle.len()] == needle[..] {
                            pool.push(a + o as u64);
                        }
                        o += 16;
                    }
                }
                a += n;
            }
        }
    }
    let used = USED.lock().unwrap().clone();
    let mut free: Vec<u64> = pool.iter().copied().filter(|p| !used.contains(p)).collect();
    if free.is_empty() {
        eprintln!("[wwd] fontfix: no Unreal-allocated path strings yet (run with --env WWD_FONT_POOL=1)");
        return;
    }

    // 1. candidate composite fonts: DefaultTypeface.Fonts TArray + flag + revision
    let mut fonts = Vec::new();
    const C: u64 = 1 << 20;
    for &(s, e) in &ranges {
        let mut a = s;
        while a < e {
            let n = (e - a).min(C);
            if let Some(b) = safe_read(a, n as usize) {
                let mut o = 0usize;
                while o + 64 <= b.len() {
                    let data: u64 = rd(&b, o);
                    let num: i32 = rd(&b, o + 8);
                    let max: i32 = rd(&b, o + 12);
                    if data != 0
                        && data & 7 == 0
                        && (1..=16).contains(&num)
                        && num <= max
                        && max <= 32
                        && rd::<u8>(&b, o + 56) <= 1
                        && (0..100_000).contains(&rd::<i32>(&b, o + 60))
                    {
                        if let Some(ent) = safe_read(data, 48 * num as usize) {
                            let mut faces = Vec::new();
                            let ok = (0..num as usize).all(|i| {
                                let x = &ent[48 * i..48 * (i + 1)];
                                let (fptr, fnum): (u64, i32) = (rd(x, 8), rd(x, 16));
                                let (hint, policy, sub): (u8, u8, i32) = (rd(x, 24), rd(x, 25), rd(x, 28));
                                let face: u64 = rd(x, 40);
                                let converted = used.contains(&fptr) && face == 0;
                                faces.push(if converted { 0 } else { face });
                                // asset-based entry (no filename), or one we already converted
                                ((fptr == 0 && fnum == 0 && face != 0 && face & 7 == 0) || converted) && hint <= 4 && policy <= 2 && (0..8).contains(&sub)
                            });
                            if ok {
                                fonts.push(Font { array: a + o as u64, entries: data, faces });
                            }
                        }
                    }
                    o += 8;
                }
            }
            a += n;
        }
    }
    // 2. keep fonts whose faces all share the dominant face class (UFontFace vtable)
    let mut vt_count: HashMap<u64, usize> = HashMap::new();
    let mut vt_of = HashMap::new();
    for f in &fonts {
        for &face in &f.faces {
            if face == 0 {
                continue;
            }
            let mut vt = safe_read(face, 8).map(|b| rd::<u64>(&b, 0)).unwrap_or(0);
            // a vtable lives in the library image, never in writable memory
            let i = ranges.partition_point(|&(s, _)| s <= vt);
            if vt & 7 != 0 || (i > 0 && vt < ranges[i - 1].1) || safe_read(vt, 8).is_none() {
                vt = 0;
            }
            vt_of.insert(face, vt);
            if vt != 0 {
                *vt_count.entry(vt).or_default() += 1;
            }
        }
    }
    let mut top: Vec<(u64, usize)> = vt_count.iter().map(|(k, v)| (*k, *v)).collect();
    top.sort_by_key(|x| std::cmp::Reverse(x.1));
    eprintln!("[wwd] fontfix: face classes {:x?}", &top[..top.len().min(6)]);
    if std::env::var_os("WWD_FONT_FIX_DRY").is_some() {
        for f in fonts.iter().filter(|f| f.faces.iter().any(|x| vt_of.get(x).copied().unwrap_or(0) == top.first().map(|t| t.0).unwrap_or(1))).take(20) {
            eprintln!("[wwd] fontfix:   font array {:#x} entries {:#x} faces {:x?}", f.array, f.entries, f.faces);
        }
        return;
    }
    let is_vtable = |vt: u64| {
        let i = ranges.partition_point(|&(s, _)| s <= vt);
        vt != 0 && vt & 7 == 0 && !(i > 0 && vt < ranges[i - 1].1) && safe_read(vt, 8).is_some()
    };
    // Pick the face class whose fonts are anchored by a UFont vtable that
    // sits right next to it (UFont and UFontFace vtables are neighbours in
    // the library's .data.rel.ro); heap clusters never qualify.
    let mut chosen = None;
    for &(face_vt, _) in top.iter().take(8) {
        let face_ok = |f: &Font| {
            let live: Vec<u64> = f.faces.iter().copied().filter(|&x| x != 0).collect();
            !live.is_empty() && live.iter().all(|face| vt_of.get(face) == Some(&face_vt))
        };
        let mut ufont_count: HashMap<u64, usize> = HashMap::new();
        let mut ufont_of = HashMap::new();
        for f in fonts.iter().filter(|f| face_ok(f) || f.faces.iter().all(|x| *x == 0)) {
            let vt = safe_read(f.array - 320, 8).map(|b| rd::<u64>(&b, 0)).unwrap_or(0);
            if is_vtable(vt) && vt.abs_diff(face_vt) < 0x10000 {
                ufont_of.insert(f.array, vt);
                *ufont_count.entry(vt).or_default() += 1;
            }
        }
        if let Some((&ufont_vt, &nfonts)) = ufont_count.iter().max_by_key(|(_, c)| **c) {
            chosen = Some((face_vt, ufont_vt, nfonts, ufont_of));
            break;
        }
    }
    let Some((face_vt, ufont_vt, nfonts, ufont_of)) = chosen else {
        eprintln!("[wwd] fontfix: no UFont objects identified");
        return;
    };
    let by_array: HashMap<u64, &Font> = fonts.iter().map(|f| (f.array, f)).collect();
    let mut changed_fonts = 0;
    let mut changed_entries = 0;
    let mut convert = |f: &Font| -> usize {
        let mut n = 0;
        for (i, &face) in f.faces.iter().enumerate() {
            if face == 0 || vt_of.get(&face) != Some(&face_vt) {
                continue;
            }
            let Some(path_ptr) = free.pop() else { break };
            USED.lock().unwrap().push(path_ptr);
            let e = f.entries + 48 * i as u64;
            safe_write(e + 8, path_ptr);
            safe_write(e + 16, path_len);
            safe_write(e + 20, path_len);
            safe_write(e + 25, 0u8); // EFontLoadingPolicy::LazyLoad
            safe_write(e + 28, KOREAN_SUBFACE);
            safe_write(e + 40, 0u64); // no FontFaceAsset: FFontData uses FontFilename
            n += 1;
        }
        n
    };
    for (&array, &vt) in &ufont_of {
        if vt != ufont_vt {
            continue;
        }
        let f = by_array[&array];
        let mut n = convert(f);
        // FallbackTypeface.Typeface.Fonts follows at +16
        if let Some(fb) = by_array.get(&(array + 16)) {
            n += convert(fb);
        }
        if n > 0 {
            let rev: i32 = safe_read(array + 60, 4).map(|b| rd(&b, 0)).unwrap_or(0);
            safe_write(array + 60, rev + 1);
            changed_fonts += 1;
            changed_entries += n;
        }
    }
    eprintln!("[wwd] fontfix: UFont class {ufont_vt:#x} ({nfonts} fonts)");
    eprintln!(
        "[wwd] fontfix: {} MB scanned; {} candidate fonts, face class {:#x} x{}; converted {} entries in {} fonts to {}#{} ({} ms)",
        total >> 20,
        fonts.len(),
        face_vt,
        vt_count[&face_vt],
        changed_entries,
        changed_fonts,
        FONT_PATH,
        KOREAN_SUBFACE,
        t0.elapsed().as_millis()
    );
}
