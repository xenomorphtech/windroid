//! Guest virtual memory on top of Win32 VirtualAlloc.
//!
//! The guest address space is identity-mapped into the host process. Linux
//! semantics are emulated as follows:
//!   * mmap(NULL)        -> one fresh reservation (64 KiB granular)
//!   * mmap(MAP_FIXED)   -> reserve any free 64 KiB granules, then commit
//!   * PROT_NONE         -> reserve only (no commit charge)
//!   * munmap            -> decommit; a reservation whose pages are all
//!                          decommitted is released
//!   * mprotect          -> commit-with-protection / VirtualProtect per region
//!
//! Overcommit: mappings larger than EAGER_LIMIT are only *reserved*; their
//! protection lives in the VMA table and pages are committed 64 KiB at a time
//! by `handle_fault` when first touched (like Linux demand paging). This keeps
//! huge PROT_READ|PROT_WRITE reservations (UE maps 48 GB) cheap, and avoids
//! VirtualQuery on giant committed regions, which walks every PTE.

use std::collections::BTreeMap;
use std::sync::RwLock;
use windows_sys::Win32::System::Memory::*;

const EAGER_LIMIT: u64 = 64 << 20;

/// Guest mappings: start -> (end, prot). Page granular, non-overlapping.
static VMAS: RwLock<BTreeMap<u64, (u64, u32)>> = RwLock::new(BTreeMap::new());

fn vma_set(start: u64, end: u64, prot: Option<u32>) {
    let mut m = VMAS.write().unwrap();
    let overlapping: Vec<(u64, (u64, u32))> = m.range(..end).filter(|(s, (e, _))| **s < end && *e > start).map(|(s, v)| (*s, *v)).collect();
    for (s0, (e0, p0)) in overlapping {
        m.remove(&s0);
        if s0 < start {
            m.insert(s0, (start, p0));
        }
        if e0 > end {
            m.insert(end, (e0, p0));
        }
    }
    if let Some(p) = prot {
        m.insert(start, (end, p));
    }
}

fn vma_find(addr: u64) -> Option<(u64, u64, u32)> {
    let m = VMAS.read().ok()?;
    let (s, (e, p)) = m.range(..=addr).next_back()?;
    if addr < *e {
        Some((*s, *e, *p))
    } else {
        None
    }
}

/// Update the protection of existing VMAs in [start, end) (mprotect).
fn vma_protect(start: u64, end: u64, prot: u32) {
    let pieces: Vec<(u64, u64)> = {
        let m = VMAS.read().unwrap();
        m.range(..end).filter(|(s, (e, _))| **s < end && *e > start).map(|(s, (e, _))| ((*s).max(start), (*e).min(end))).collect()
    };
    for (a, b) in pieces {
        vma_set(a, b, Some(prot));
    }
}

/// Commit lazily reserved guest pages in [addr, addr+len) before the host
/// kernel writes into them (ReadFile & co. fail with ERROR_NOACCESS on
/// uncommitted memory instead of raising a fault we could service).
pub fn prefault(addr: u64, len: u64) {
    if len == 0 {
        return;
    }
    let addr = untag(addr);
    let mut a = align_down(addr, GRANULE);
    while a < addr + len {
        handle_fault(a.max(addr), false);
        a += GRANULE;
    }
}

/// Demand paging: commit the chunk around `addr` if it lies in a guest
/// mapping whose protection allows the access. Called from the exception
/// handler; returns true if execution can simply resume.
pub fn handle_fault(addr: u64, write: bool) -> bool {
    let addr = untag(addr);
    let Some((s, e, prot)) = vma_find(addr) else { return false };
    if prot == 0 || (write && prot & PROT_WRITE == 0) {
        return false;
    }
    match query(addr) {
        Some(r) if r.state == MEM_RESERVE => {}
        Some(r) if r.state == MEM_COMMIT => {
            // Another thread may have committed this granule between our
            // fault and now: resume if the page already permits the access.
            let ok = if write {
                matches!(r.protect & 0xff, PAGE_READWRITE | PAGE_EXECUTE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_WRITECOPY)
            } else {
                !matches!(r.protect & 0xff, PAGE_NOACCESS) && r.protect & PAGE_GUARD == 0
            };
            return ok;
        }
        _ => return false,
    }
    let lo = align_down(addr, GRANULE).max(s);
    let hi = (align_down(addr, GRANULE) + GRANULE).min(e);
    // commit only the reserved runs (committed neighbours keep their contents)
    for_each_region(lo, hi - lo, |a, l, r| {
        if r.state == MEM_RESERVE {
            unsafe { VirtualAlloc(a as *const _, l as usize, MEM_COMMIT, win_prot(prot)) };
        }
        true
    });
    true
}

pub const PAGE: u64 = 4096;
pub const GRANULE: u64 = 65536;

pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC: u32 = 4;

#[inline]
pub fn align_down(v: u64, a: u64) -> u64 {
    v & !(a - 1)
}
#[inline]
pub fn align_up(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

fn win_prot(prot: u32) -> u32 {
    // The JIT only *reads* guest code, so EXEC never needs host execute rights.
    if prot & PROT_WRITE != 0 {
        PAGE_READWRITE
    } else if prot & (PROT_READ | PROT_EXEC) != 0 {
        PAGE_READONLY
    } else {
        PAGE_NOACCESS
    }
}

struct Region {
    base: u64,
    size: u64,
    alloc_base: u64,
    state: u32,
    protect: u32,
}

fn query(addr: u64) -> Option<Region> {
    let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let n = unsafe { VirtualQuery(addr as *const _, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
    if n == 0 {
        return None;
    }
    Some(Region { base: mbi.BaseAddress as u64, size: mbi.RegionSize as u64, alloc_base: mbi.AllocationBase as u64, state: mbi.State, protect: mbi.Protect })
}

/// Visit [addr, addr+len) split at Windows region boundaries.
fn for_each_region(addr: u64, len: u64, mut f: impl FnMut(u64, u64, &Region) -> bool) -> bool {
    let end = addr + len;
    let mut a = addr;
    while a < end {
        let Some(r) = query(a) else { return false };
        let rend = (r.base + r.size).min(end);
        if !f(a, rend - a, &r) {
            return false;
        }
        a = rend;
    }
    true
}

/// Make sure every 64 KiB granule touching [addr, addr+len) is reserved.
fn reserve_range(addr: u64, len: u64) -> bool {
    let want_end = addr + len;
    let end = align_up(want_end, GRANULE);
    let mut a = align_down(addr, GRANULE);
    while a < end {
        let Some(r) = query(a) else { return false };
        let r_end = (r.base + r.size).min(end);
        if r.state == MEM_FREE {
            // A reservation must start on a 64 KiB boundary. A free region that
            // starts mid-granule is the unusable tail of a neighbour allocation.
            let start = align_up(a, GRANULE);
            if start > a && a < want_end && start.min(r_end) > addr {
                return false;
            }
            if start < r_end {
                let p = unsafe { VirtualAlloc(start as *const _, (r_end - start) as usize, MEM_RESERVE, PAGE_NOACCESS) };
                if p.is_null() {
                    return false;
                }
            }
        }
        a = r_end;
    }
    true
}

fn commit(addr: u64, len: u64, prot: u32) -> bool {
    for_each_region(addr, len, |a, l, _| unsafe { !VirtualAlloc(a as *const _, l as usize, MEM_COMMIT, win_prot(prot)).is_null() })
}

/// Release reservations that lie entirely inside [addr, addr+len) and have no
/// committed pages left. A partially covered reservation stays reserved: the
/// guest's other mappings in it (e.g. a PROT_NONE arena) must survive.
fn release_empty(addr: u64, len: u64) {
    let end = addr + len;
    let mut bases = Vec::new();
    for_each_region(addr, len, |_, _, r| {
        if r.state != MEM_FREE && !bases.contains(&r.alloc_base) {
            bases.push(r.alloc_base);
        }
        true
    });
    for b in bases {
        // walk the whole allocation; release only if fully covered and empty
        let mut a = b;
        let mut any_committed = false;
        loop {
            let Some(r) = query(a) else { break };
            if r.alloc_base != b || r.state == MEM_FREE {
                break;
            }
            if r.state == MEM_COMMIT {
                any_committed = true;
                break;
            }
            a = r.base + r.size;
        }
        let covered = b >= addr && a <= end;
        if !any_committed && covered {
            unsafe { VirtualFree(b as *mut _, 0, MEM_RELEASE) };
        }
    }
}

pub fn is_free(addr: u64, len: u64) -> bool {
    for_each_region(align_down(addr, GRANULE), align_up(addr + len, GRANULE) - align_down(addr, GRANULE), |_, _, r| r.state == MEM_FREE)
}

/// Anonymous mapping. Returns the address or a negative errno.
pub fn map_anon(hint: u64, len: u64, prot: u32, fixed: bool) -> i64 {
    let len = align_up(len, PAGE);
    if len == 0 {
        return -crate::abi::EINVAL;
    }
    if fixed {
        if hint & (PAGE - 1) != 0 {
            return -crate::abi::EINVAL;
        }
        // drop whatever was there (decommit only), then provide fresh zero pages
        decommit(hint, len);
        if !reserve_range(hint, len) {
            return -crate::abi::ENOMEM;
        }
        if prot != 0 && len <= EAGER_LIMIT && !commit(hint, len, prot) {
            return -crate::abi::ENOMEM;
        }
        vma_set(hint, hint + len, Some(prot));
        return hint as i64;
    }
    let size = align_up(len, GRANULE);
    let mut p = std::ptr::null_mut();
    if hint != 0 && hint & (GRANULE - 1) == 0 && is_free(hint, size) {
        p = unsafe { VirtualAlloc(hint as *const _, size as usize, MEM_RESERVE, PAGE_NOACCESS) };
    }
    if p.is_null() {
        p = unsafe { VirtualAlloc(std::ptr::null(), size as usize, MEM_RESERVE, PAGE_NOACCESS) };
    }
    if p.is_null() {
        return -crate::abi::ENOMEM;
    }
    let a = p as u64;
    if prot != 0 && len <= EAGER_LIMIT && !commit(a, len, prot) {
        unsafe { VirtualFree(p, 0, MEM_RELEASE) };
        return -crate::abi::ENOMEM;
    }
    vma_set(a, a + len, Some(prot));
    a as i64
}

fn decommit(addr: u64, len: u64) {
    for_each_region(addr, len, |a, l, r| {
        if r.state == MEM_COMMIT {
            unsafe { VirtualFree(a as *mut _, l as usize, MEM_DECOMMIT) };
        }
        true
    });
}

pub fn unmap(addr: u64, len: u64) -> i64 {
    if addr & (PAGE - 1) != 0 {
        return -crate::abi::EINVAL;
    }
    let len = align_up(len, PAGE);
    vma_set(addr, addr + len, None);
    for_each_region(addr, len, |a, l, r| {
        if r.state == MEM_COMMIT {
            unsafe { VirtualFree(a as *mut _, l as usize, MEM_DECOMMIT) };
        }
        true
    });
    release_empty(addr, len);
    0
}

pub fn protect(addr: u64, len: u64, prot: u32) -> i64 {
    if addr & (PAGE - 1) != 0 {
        return -crate::abi::EINVAL;
    }
    let len = align_up(len, PAGE);
    vma_protect(addr, addr + len, prot);
    let lazy = len > EAGER_LIMIT;
    let ok = for_each_region(addr, len, |a, l, r| {
        if r.state == MEM_FREE {
            return false;
        }
        unsafe {
            if r.state == MEM_RESERVE && lazy {
                true // committed on first touch with the new protection
            } else if prot == 0 {
                if r.state == MEM_COMMIT {
                    let mut old = 0;
                    VirtualProtect(a as *const _, l as usize, PAGE_NOACCESS, &mut old);
                }
                true
            } else if r.state == MEM_RESERVE {
                !VirtualAlloc(a as *const _, l as usize, MEM_COMMIT, win_prot(prot)).is_null()
            } else {
                let mut old = 0;
                VirtualProtect(a as *const _, l as usize, win_prot(prot), &mut old) != 0
            }
        }
    });
    if ok {
        0
    } else {
        -crate::abi::ENOMEM
    }
}

/// MADV_DONTNEED: committed pages in the range must read back as zero.
pub fn discard(addr: u64, len: u64) {
    let addr = align_down(addr, PAGE);
    let len = align_up(len, PAGE);
    // Decommit: the pages come back zero-filled on the next touch through
    // demand paging (only for ranges the VMA table knows about).
    if vma_find(addr).is_none() {
        return;
    }
    if addr / GRANULE == (addr + len - 1) / GRANULE {
        unsafe { VirtualFree(addr as *mut _, len as usize, MEM_DECOMMIT) };
        return;
    }
    for_each_region(addr, len, |a, l, r| {
        if r.state == MEM_COMMIT {
            unsafe { VirtualFree(a as *mut _, l as usize, MEM_DECOMMIT) };
        }
        true
    });
}

/// Program break, backed by one large reservation committed on demand.
pub struct Brk {
    pub base: u64,
    pub cur: u64,
    pub limit: u64,
}

impl Brk {
    pub fn new(image_end: u64) -> Brk {
        const SIZE: u64 = 512 << 20;
        let base = align_up(image_end, GRANULE);
        let p = unsafe { VirtualAlloc(base as *const _, SIZE as usize, MEM_RESERVE, PAGE_NOACCESS) };
        if p.is_null() {
            // no room right after the image: brk will always fail; libc falls back to mmap
            return Brk { base, cur: base, limit: base };
        }
        Brk { base, cur: base, limit: base + SIZE }
    }

    pub fn set(&mut self, new: u64) -> u64 {
        if new == 0 || new < self.base || new > self.limit {
            return self.cur;
        }
        let old_top = align_up(self.cur, PAGE);
        let new_top = align_up(new, PAGE);
        if new_top > old_top {
            if !commit(old_top, new_top - old_top, PROT_READ | PROT_WRITE) {
                return self.cur;
            }
            vma_set(old_top, new_top, Some(PROT_READ | PROT_WRITE));
        } else if new_top < old_top {
            vma_set(new_top, old_top, None);
            unsafe { VirtualFree(new_top as *mut _, (old_top - new_top) as usize, MEM_DECOMMIT) };
        }
        self.cur = new;
        new
    }
}

/// Validate that [addr, addr+len) is committed and readable.
pub fn readable(addr: u64, len: u64) -> bool {
    if len == 0 {
        return true;
    }
    if addr == 0 {
        return false;
    }
    let addr = untag(addr);
    // guest mappings: readable per protection (lazily committed pages count)
    if let Some((_, e, prot)) = vma_find(addr) {
        if addr + len <= e {
            return prot & (PROT_READ | PROT_EXEC) != 0;
        }
    }
    for_each_region(addr, len, |_, _, r| r.state == MEM_COMMIT)
}

pub unsafe fn slice<'a>(addr: u64, len: u64) -> &'a [u8] {
    if len == 0 {
        return &[];
    }
    std::slice::from_raw_parts(untag(addr) as *const u8, len as usize)
}

pub unsafe fn slice_mut<'a>(addr: u64, len: u64) -> &'a mut [u8] {
    if len == 0 {
        return &mut [];
    }
    std::slice::from_raw_parts_mut(untag(addr) as *mut u8, len as usize)
}

/// Strip an arm64 TBI pointer tag.
pub fn untag(addr: u64) -> u64 {
    addr & 0x00ff_ffff_ffff_ffff
}

pub unsafe fn cstr(addr: u64) -> Vec<u8> {
    let mut v = Vec::new();
    let mut p = untag(addr) as *const u8;
    while *p != 0 {
        v.push(*p);
        p = p.add(1);
    }
    v
}

pub unsafe fn write_obj<T: Copy>(addr: u64, v: T) {
    (untag(addr) as *mut T).write_unaligned(v)
}

pub unsafe fn read_obj<T: Copy>(addr: u64) -> T {
    (untag(addr) as *const T).read_unaligned()
}

/// Committed, readable+writable guest ranges (for debug scanners).
pub fn committed_rw_ranges() -> Vec<(u64, u64)> {
    let vmas: Vec<(u64, u64, u32)> = VMAS.read().unwrap().iter().map(|(s, (e, p))| (*s, *e, *p)).collect();
    let mut out = Vec::new();
    for (s, e, p) in vmas {
        if p & (PROT_READ | PROT_WRITE) != (PROT_READ | PROT_WRITE) {
            continue;
        }
        for_each_region(s, e - s, |a, l, r| {
            if r.state == MEM_COMMIT && matches!(r.protect & 0xff, PAGE_READWRITE | PAGE_EXECUTE_READWRITE) && r.protect & PAGE_GUARD == 0 {
                out.push((a, a + l));
            }
            true
        });
    }
    out
}
