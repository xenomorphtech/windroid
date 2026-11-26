//! wwd-cpu: AArch64 guest CPU for the wwd Android runtime.
//!
//! * `decode`  — A64 instruction decoder
//! * `interp`  — reference interpreter (complete semantics)
//! * `simd`    — Advanced SIMD / FP semantics
//! * `jit`     — AArch64 -> x86-64 block translator
//! * `Engine`  — runs a `Cpu` until it needs the OS (syscall, fault, ...)

pub mod cpu;
pub mod decode;
pub mod guard;
pub mod interp;
pub mod jit;
pub mod simd;

pub use cpu::{Cpu, ExitReason};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Pure interpreter: slow, but the reference for correctness.
    Interp,
    /// Block JIT with interpreter fallback.
    Jit,
}

pub struct Engine {
    jit: Option<jit::Jit>,
    pub interp_steps: u64,
    /// step over the breakpoint we just stopped at
    resume_over: bool,
}

/// Debug breakpoints (absolute guest addresses); honoured in interpreter mode.
pub static BREAKPOINTS: std::sync::RwLock<Vec<u64>> = std::sync::RwLock::new(Vec::new());

impl Engine {
    pub fn new(mode: Mode) -> Engine {
        Engine { jit: if mode == Mode::Jit { Some(jit::Jit::new()) } else { None }, interp_steps: 0, resume_over: false }
    }

    /// Run until the guest needs the host (SVC, BRK, undefined instruction,
    /// memory fault...). Host faults inside guest execution are contained and
    /// reported as `ExitReason::Fault`.
    pub fn run(&mut self, cpu: &mut Cpu) -> ExitReason {
        loop {
            cpu.exit_reason = ExitReason::None as u32;
            ACTIVE.with(|a| a.set(self as *mut Engine));
            let jit = self.jit.as_ref().map(|j| j as *const jit::Jit).unwrap_or(std::ptr::null());
            guard::guarded(cpu, jit, run_loop);
            let r = reason(cpu.exit_reason);
            if r == ExitReason::CacheFlush {
                self.flush();
                continue;
            }
            return r;
        }
    }

    /// Invalidate translated code (e.g. after munmap/mprotect of guest code).
    pub fn flush(&mut self) {
        if let Some(j) = &mut self.jit {
            j.flush();
        }
    }

    pub fn stats(&self) -> Option<&'static jit::cache::Stats> {
        self.jit.as_ref().map(|_| &jit::cache::get().stats)
    }
}

thread_local! {
    static ACTIVE: std::cell::Cell<*mut Engine> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

/// The dispatcher loop proper; runs inside the fault guard.
unsafe extern "sysv64" fn run_loop(cpu: *mut Cpu) {
    let cpu = &mut *cpu;
    let eng = &mut *ACTIVE.with(|a| a.get());
    match &mut eng.jit {
        Some(jit) => loop {
            let entry = jit.lookup(cpu.pc);
            if cpu.chain_slot != 0 {
                jit::cache::get().link(cpu.chain_slot, entry + jit::BODY_OFFSET);
                cpu.chain_slot = 0;
            }
            let f: jit::BlockFn = std::mem::transmute(entry);
            f(cpu as *mut Cpu);
            if cpu.exit_reason != 0 {
                cpu.chain_slot = 0;
                return;
            }
        },
        None => {
            let bps = BREAKPOINTS.read().unwrap().clone();
            let mut skip = std::mem::take(&mut eng.resume_over);
            loop {
                if !bps.is_empty() && !skip && bps.contains(&cpu.pc) {
                    cpu.exit_reason = ExitReason::Breakpoint as u32;
                    eng.resume_over = true;
                    return;
                }
                skip = false;
                eng.interp_steps += 1;
                if !interp::step(cpu) {
                    return;
                }
            }
        }
    }
}

/// Guest code in [lo, lo+len) may have changed: drop translations if any exist there.
pub fn invalidate_code(lo: u64, len: u64) -> bool {
    jit::cache::get().invalidate(lo, len)
}

/// Guest PC for a host instruction pointer inside translated code (profiling, faults).
pub fn guest_pc_for_host(rip: u64) -> Option<u64> {
    jit::cache::get().guest_pc_for_host(rip)
}

fn reason(r: u32) -> ExitReason {
    match r {
        1 => ExitReason::Svc,
        2 => ExitReason::Brk,
        3 => ExitReason::Undefined,
        4 => ExitReason::CacheFlush,
        5 => ExitReason::Fault,
        6 => ExitReason::Breakpoint,
        _ => ExitReason::None,
    }
}

/// Static coverage scan: decode every word of `code` (loaded at `base`) and
/// report encodings the CPU cannot execute, grouped by opcode pattern
/// (register fields masked). SIMD data-processing words are trial-executed on
/// a scratch CPU, which is safe because they never touch memory.
pub fn coverage(code: &[u8], base: u64) -> Vec<(u32, usize, u32, u64)> {
    use std::collections::HashMap;
    let mut groups: HashMap<u32, (usize, u32, u64)> = HashMap::new();
    let mut scratch = Cpu::default();
    for (i, c) in code.chunks_exact(4).enumerate() {
        let w = u32::from_le_bytes(c.try_into().unwrap());
        let pc = base + i as u64 * 4;
        let bad = match decode::decode(w, pc) {
            decode::Insn::Unknown(_) => true,
            decode::Insn::Simd(w) => {
                scratch.pc = pc;
                !simd::exec(&mut scratch, w)
            }
            _ => false,
        };
        if bad {
            let key = w & 0xffe0_fc00; // drop Rd, Rn, Rm
            let e = groups.entry(key).or_insert((0, w, pc));
            e.0 += 1;
        }
    }
    let mut v: Vec<_> = groups.into_iter().map(|(k, (n, w, pc))| (k, n, w, pc)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}
