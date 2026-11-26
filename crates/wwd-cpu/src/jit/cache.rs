//! Process-wide translation cache shared by all guest threads.
//!
//! One large reservation, committed on demand:
//!   [0, TABLE)              indirect-branch hash table: (guest pc, host body) pairs
//!   [TABLE, TABLE+SLOTS)    exit slots: 8-byte jump targets for chained exits
//!   [CODE, END)             translated code (position-independent, append-only)
//!
//! Chaining: a direct exit is `mov rax, slot; jmp [rax]`. The slot first points
//! at the exit's own stub, which records the slot and returns to the
//! dispatcher. The dispatcher then stores the successor's body address into the
//! slot: one aligned 8-byte store, so threads running the code concurrently
//! never see a torn target.
//!
//! Code memory is never reused: a flush unlinks every slot and clears the
//! lookup structures, but blocks other threads may still be executing stay valid.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use windows_sys::Win32::System::Memory::*;

pub const TABLE_BITS: u32 = 20;
pub const TABLE_ENTRIES: u64 = 1 << TABLE_BITS;
const TABLE_SIZE: u64 = TABLE_ENTRIES * 16;
const SLOTS_SIZE: u64 = 64 << 20;
const CODE_OFF: u64 = TABLE_SIZE + SLOTS_SIZE;
const REGION: u64 = 3 << 30;
const COMMIT_STEP: u64 = 4 << 20;

pub struct Cache {
    pub base: u64,
    slot_next: AtomicU64,
    slot_committed: Mutex<u64>,
    inner: Mutex<Inner>,
    pub map: RwLock<HashMap<u64, u64>>,
    /// host block start -> (host end, [(host ip, guest pc)])
    ranges: RwLock<BTreeMap<u64, (u64, Vec<(u64, u64)>)>>,
    /// bumped on flush; threads drop their local lookup caches when it changes
    pub generation: AtomicU64,
    /// 4 KiB guest pages that currently have translations
    pages: RwLock<BTreeSet<u64>>,
    pub stats: Stats,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub blocks: AtomicU64,
    pub guest_insns: AtomicU64,
    pub native_insns: AtomicU64,
    pub fallback_insns: AtomicU64,
    pub chains: AtomicU64,
    pub flushes: AtomicU64,
    pub code_bytes: AtomicU64,
}

struct Inner {
    code_used: u64,
    code_committed: u64,
    /// every slot with its initial (stub) target, for unlinking on flush
    slots: Vec<(u64, u64)>,
}

static CACHE: OnceLock<Cache> = OnceLock::new();

pub fn get() -> &'static Cache {
    CACHE.get_or_init(Cache::new)
}

impl Cache {
    fn new() -> Cache {
        let base = unsafe { VirtualAlloc(std::ptr::null(), REGION as usize, MEM_RESERVE, PAGE_NOACCESS) } as u64;
        assert!(base != 0, "cannot reserve JIT region");
        let ok = unsafe { VirtualAlloc(base as *const _, TABLE_SIZE as usize, MEM_COMMIT, PAGE_READWRITE) };
        assert!(!ok.is_null(), "cannot commit JIT lookup table");
        Cache {
            base,
            slot_next: AtomicU64::new(base + TABLE_SIZE),
            slot_committed: Mutex::new(base + TABLE_SIZE),
            inner: Mutex::new(Inner { code_used: 0, code_committed: 0, slots: Vec::new() }),
            map: RwLock::new(HashMap::new()),
            ranges: RwLock::new(BTreeMap::new()),
            generation: AtomicU64::new(0),
            pages: RwLock::new(BTreeSet::new()),
            stats: Stats::default(),
        }
    }

    pub fn table(&self) -> u64 {
        self.base
    }

    /// Allocate a zeroed, committed 8-byte exit slot (lock-free fast path).
    pub fn alloc_slot(&self) -> u64 {
        let s = self.slot_next.fetch_add(8, Ordering::Relaxed);
        assert!(s + 8 <= self.base + TABLE_SIZE + SLOTS_SIZE, "JIT exit slots exhausted");
        let mut c = self.slot_committed.lock().unwrap();
        while s + 8 > *c {
            let p = unsafe { VirtualAlloc(*c as *const _, COMMIT_STEP as usize, MEM_COMMIT, PAGE_READWRITE) };
            assert!(!p.is_null(), "cannot commit JIT slots");
            *c += COMMIT_STEP;
        }
        s
    }

    /// Copy position-independent `code` into the cache and publish it.
    /// `fix` gets the final base address to resolve stub slots before publishing.
    pub fn install(&self, pc: u64, code: &[u8], pcs: Vec<(u64, u64)>, stubs: Vec<(u64, u64)>) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        let addr = self.base + CODE_OFF + inner.code_used;
        let need = inner.code_used + code.len() as u64;
        assert!(CODE_OFF + need <= REGION, "JIT code cache full");
        while need > inner.code_committed {
            let a = self.base + CODE_OFF + inner.code_committed;
            let p = unsafe { VirtualAlloc(a as *const _, COMMIT_STEP as usize, MEM_COMMIT, PAGE_EXECUTE_READWRITE) };
            assert!(!p.is_null(), "cannot commit JIT code");
            inner.code_committed += COMMIT_STEP;
        }
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), addr as *mut u8, code.len()) };
        inner.code_used = (need + 15) & !15;
        for (slot, stub_off) in stubs {
            let stub = addr + stub_off;
            unsafe { (*(slot as *const AtomicU64)).store(stub, Ordering::Release) };
            inner.slots.push((slot, stub));
        }
        if let (Some(lo), Some(hi)) = (pcs.iter().map(|p| p.1).min(), pcs.iter().map(|p| p.1).max()) {
            let mut pages = self.pages.write().unwrap();
            let mut pg = lo.min(pc) & !0xfff;
            while pg <= hi.max(pc) {
                pages.insert(pg);
                pg += 0x1000;
            }
        }
        let pcs = pcs.into_iter().map(|(o, g)| (addr + o, g)).collect();
        self.ranges.write().unwrap().insert(addr, (addr + code.len() as u64, pcs));
        // publish (another thread may have raced us translating the same pc;
        // keep the first so chained slots stay consistent)
        let mut map = self.map.write().unwrap();
        let entry = *map.entry(pc).or_insert(addr);
        drop(map);
        if entry == addr {
            self.table_insert(pc, addr + super::BODY_OFFSET);
            self.stats.blocks.fetch_add(1, Ordering::Relaxed);
            self.stats.code_bytes.fetch_add(code.len() as u64, Ordering::Relaxed);
        }
        entry
    }

    #[inline]
    pub fn table_slot(&self, pc: u64) -> u64 {
        self.base + (((pc >> 2) & (TABLE_ENTRIES - 1)) << 4)
    }

    /// Fill an empty table entry only: overwriting a live (pc, host) pair
    /// would race with readers.
    fn table_insert(&self, pc: u64, body: u64) {
        let e = self.table_slot(pc);
        unsafe {
            let k = &*(e as *const AtomicU64);
            let v = &*((e + 8) as *const AtomicU64);
            if k.load(Ordering::Acquire) == 0 {
                v.store(body, Ordering::Release);
                k.store(pc, Ordering::Release);
            }
        }
    }

    /// Point a chained exit slot at `body`.
    pub fn link(&self, slot: u64, body: u64) {
        unsafe { (*(slot as *const AtomicU64)).store(body, Ordering::Release) };
        self.stats.chains.fetch_add(1, Ordering::Relaxed);
    }

    /// Flush if [lo, lo+len) overlaps any translated guest page.
    /// Returns true if a flush happened.
    pub fn invalidate(&self, lo: u64, len: u64) -> bool {
        if len == 0 {
            return false;
        }
        let hit = {
            let pages = self.pages.read().unwrap();
            pages.range((lo & !0xfff)..lo.saturating_add(len)).next().is_some()
        };
        if hit {
            self.flush();
        }
        hit
    }

    /// Invalidate all translations (guest code changed).
    pub fn flush(&self) {
        let inner = self.inner.lock().unwrap();
        self.map.write().unwrap().clear();
        self.ranges.write().unwrap().clear();
        self.pages.write().unwrap().clear();
        for &(slot, stub) in &inner.slots {
            unsafe { (*(slot as *const AtomicU64)).store(stub, Ordering::Release) };
        }
        for i in 0..TABLE_ENTRIES {
            unsafe { (*((self.base + i * 16) as *const AtomicU64)).store(0, Ordering::Release) };
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.stats.flushes.fetch_add(1, Ordering::Relaxed);
    }

    /// Guest PC whose translation contains host address `rip` (fault handler).
    /// Uses try_read: the faulting thread may hold the lock itself.
    pub fn guest_pc_for_host(&self, rip: u64) -> Option<u64> {
        let r = self.ranges.try_read().ok()?;
        let (_, (end, pcs)) = r.range(..=rip).next_back()?;
        if rip >= *end {
            return None;
        }
        let i = match pcs.binary_search_by(|(ip, _)| ip.cmp(&rip)) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        Some(pcs[i].1)
    }
}
