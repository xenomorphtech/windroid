//! Signal delivery with real arm64 `rt_sigframe` layouts.
//!
//! Frame (growing down from the chosen stack):
//!   sp + 0      siginfo_t           (128 bytes)
//!   sp + 128    struct ucontext     (4560 bytes; uc_mcontext at +176)
//!   sp + 4688   frame record        {x29, x30}
//! ucontext.uc_mcontext.__reserved holds an fpsimd_context and a terminator,
//! exactly as the kernel writes it, so libc/ART/unwinders can parse it.

use crate::mem::{read_obj, write_obj};
use crate::{exit_process, Thread};
use std::sync::atomic::Ordering;

pub const SIGILL: i32 = 4;
pub const SIGTRAP: i32 = 5;
pub const SIGABRT: i32 = 6;
pub const SIGBUS: i32 = 7;
pub const SIGFPE: i32 = 8;
pub const SIGKILL: i32 = 9;
pub const SIGSEGV: i32 = 11;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGSTOP: i32 = 19;
pub const SIGURG: i32 = 23;
pub const SIGWINCH: i32 = 28;

pub const SA_SIGINFO: u64 = 0x4;
pub const SA_ONSTACK: u64 = 0x0800_0000;
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;
pub const SA_RESTORER: u64 = 0x0400_0000;

pub const SS_ONSTACK: u32 = 1;
pub const SS_DISABLE: u32 = 2;

const SIGINFO_SIZE: u64 = 128;
const UC_SIZE: u64 = 4560;
const UC_MCONTEXT: u64 = 176;
const MC_REGS: u64 = 8;
const MC_SP: u64 = 256;
const MC_PC: u64 = 264;
const MC_PSTATE: u64 = 272;
const MC_RESERVED: u64 = 288;
const FPSIMD_MAGIC: u32 = 0x4650_8001;
const FRAME_SIZE: u64 = SIGINFO_SIZE + UC_SIZE + 16;

#[derive(Clone, Copy, Debug)]
pub struct SigInfo {
    pub signo: i32,
    pub code: i32,
    /// si_addr for faults, (pid | uid << 32) for kill
    pub value: u64,
    /// synchronous fault: can't be ignored or blocked
    pub sync: bool,
}

#[derive(Clone, Copy, Default)]
pub struct AltStack {
    pub sp: u64,
    pub size: u64,
    pub flags: u32,
}

fn name(sig: i32) -> &'static str {
    match sig {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => "signal",
    }
}

fn default_ignored(sig: i32) -> bool {
    matches!(sig, SIGCHLD | SIGCONT | SIGURG | SIGWINCH)
}

fn die(t: &Thread, info: &SigInfo) -> ! {
    eprintln!(
        "[wwd] {} ({}) code={} addr/value={:#x} at pc={:#x}, terminating",
        name(info.signo),
        info.signo,
        info.code,
        info.value,
        t.cpu.pc
    );
    if info.sync {
        eprint!("{}", t.cpu.dump());
    }
    backtrace(t);
    exit_process(&t.proc, 128 + info.signo)
}

/// Guest address of a tiny `mov x8, #139; svc #0` trampoline, used as the
/// return address when the handler has no SA_RESTORER (glibc relies on vDSO).
fn sigreturn_trampoline(t: &Thread) -> u64 {
    let cur = t.proc.sigtramp.load(Ordering::Acquire);
    if cur != 0 {
        return cur;
    }
    let p = crate::mem::map_anon(0, 4096, crate::mem::PROT_READ | crate::mem::PROT_WRITE, false) as u64;
    unsafe {
        write_obj(p, 0xd280_1168u32); // movz x8, #139
        write_obj(p + 4, 0xd400_0001u32); // svc #0
    }
    crate::mem::protect(p, 4096, crate::mem::PROT_READ | crate::mem::PROT_EXEC);
    match t.proc.sigtramp.compare_exchange(0, p, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => p,
        Err(other) => other,
    }
}

/// Deliver `info` to thread `t`: run the guest handler, ignore, or terminate.
pub fn deliver(t: &mut Thread, info: SigInfo) {
    let sig = info.signo;
    if std::env::var_os("WWD_TRACE_SIGNALS").is_some() {
        eprintln!("[wwd] deliver signal {} code={} value={:#x} to tid {} at {}", sig, info.code, info.value, t.tid, where_is(t, t.cpu.pc));
        backtrace(t);
    }
    let act = t.proc.sigactions.lock().unwrap()[sig as usize];
    let blocked = t.sigmask & (1u64 << (sig - 1)) != 0;
    if sig == SIGKILL || sig == SIGSTOP {
        die(t, &info);
    }
    if !info.sync && blocked {
        t.proc.pending.lock().unwrap().push((t.tid, info));
        return;
    }
    match act.handler {
        0 => {
            if default_ignored(sig) {
                return;
            }
            die(t, &info);
        }
        1 => {
            if info.sync {
                die(t, &info); // ignoring a synchronous fault would loop forever
            }
            return;
        }
        _ if info.sync && blocked => die(t, &info),
        _ => {}
    }

    // choose the stack
    let cpu = &mut t.cpu;
    let mut sp = cpu.sp();
    let on_alt = t.altstack.flags & SS_DISABLE == 0 && t.altstack.size != 0 && sp > t.altstack.sp && sp <= t.altstack.sp + t.altstack.size;
    if act.flags & SA_ONSTACK != 0 && t.altstack.flags & SS_DISABLE == 0 && t.altstack.size != 0 && !on_alt {
        sp = t.altstack.sp + t.altstack.size;
    }
    sp = (sp - FRAME_SIZE) & !15;
    let info_p = sp;
    let uc = sp + SIGINFO_SIZE;
    let mc = uc + UC_MCONTEXT;
    unsafe {
        std::ptr::write_bytes(sp as *mut u8, 0, FRAME_SIZE as usize);
        // siginfo
        write_obj(info_p, info.signo);
        write_obj(info_p + 8, info.code);
        write_obj(info_p + 16, info.value);
        // ucontext header
        let ss_flags = if on_alt { SS_ONSTACK } else if t.altstack.size == 0 { SS_DISABLE } else { 0 };
        write_obj(uc + 16, t.altstack.sp);
        write_obj(uc + 24, ss_flags);
        write_obj(uc + 32, t.altstack.size);
        write_obj(uc + 40, t.sigmask);
        // mcontext
        write_obj(mc, if info.sync && matches!(sig, SIGSEGV | SIGBUS) { info.value } else { 0 });
        for i in 0..31 {
            write_obj(mc + MC_REGS + i * 8, cpu.x[i as usize]);
        }
        write_obj(mc + MC_SP, cpu.sp());
        write_obj(mc + MC_PC, cpu.pc);
        write_obj(mc + MC_PSTATE, cpu.nzcv << 28);
        let fp = mc + MC_RESERVED;
        write_obj(fp, FPSIMD_MAGIC);
        write_obj(fp + 4, 528u32);
        write_obj(fp + 8, cpu.fpsr as u32);
        write_obj(fp + 12, cpu.fpcr as u32);
        for i in 0..32 {
            write_obj(fp + 16 + i * 16, cpu.v[i as usize]);
        }
        // terminator {0,0} follows (already zeroed)
        let rec = uc + UC_SIZE;
        write_obj(rec, cpu.x[29]);
        write_obj(rec + 8, cpu.x[30]);

        cpu.x[0] = sig as u64;
        cpu.x[1] = info_p;
        cpu.x[2] = uc;
        cpu.x[29] = rec;
    }
    let restorer = if act.flags & SA_RESTORER != 0 && act.restorer != 0 { act.restorer } else { sigreturn_trampoline(t) };
    let cpu = &mut t.cpu;
    cpu.x[30] = restorer;
    cpu.set_sp(sp);
    cpu.pc = act.handler;
    cpu.nzcv = 0;

    if act.flags & SA_NODEFER == 0 {
        t.sigmask |= 1u64 << (sig - 1);
    }
    t.sigmask |= act.mask;
    t.sigmask &= !((1u64 << (SIGKILL - 1)) | (1u64 << (SIGSTOP - 1)));
    if act.flags & SA_RESETHAND != 0 {
        t.proc.sigactions.lock().unwrap()[sig as usize].handler = 0;
    }
}

/// rt_sigreturn: restore the context saved by `deliver` (possibly edited by the handler).
pub fn sigreturn(t: &mut Thread) {
    let sp = t.cpu.sp();
    let uc = sp + SIGINFO_SIZE;
    let mc = uc + UC_MCONTEXT;
    unsafe {
        t.sigmask = read_obj::<u64>(uc + 40) & !((1u64 << (SIGKILL - 1)) | (1u64 << (SIGSTOP - 1)));
        for i in 0..31 {
            t.cpu.x[i] = read_obj(mc + MC_REGS + i as u64 * 8);
        }
        let new_sp: u64 = read_obj(mc + MC_SP);
        t.cpu.set_sp(new_sp);
        t.cpu.pc = read_obj(mc + MC_PC);
        t.cpu.nzcv = (read_obj::<u64>(mc + MC_PSTATE) >> 28) & 15;
        // walk __reserved for the fpsimd record
        let mut p = mc + MC_RESERVED;
        let end = p + 4096;
        while p + 8 <= end {
            let magic: u32 = read_obj(p);
            let size: u32 = read_obj(p + 4);
            if magic == 0 || size < 8 {
                break;
            }
            if magic == FPSIMD_MAGIC {
                t.cpu.fpsr = read_obj::<u32>(p + 8) as u64;
                t.cpu.fpcr = read_obj::<u32>(p + 12) as u64;
                for i in 0..32 {
                    t.cpu.v[i] = read_obj(p + 16 + i as u64 * 16);
                }
            }
            p += size as u64;
        }
    }
}

/// Deliver any signals queued for this thread that are now unblocked.
pub fn deliver_pending(t: &mut Thread) {
    loop {
        let next = {
            let mut q = t.proc.pending.lock().unwrap();
            let mask = t.sigmask;
            let tid = t.tid;
            match q.iter().position(|(d, s)| *d == tid && mask & (1u64 << (s.signo - 1)) == 0) {
                Some(i) => Some(q.remove(i).1),
                None => None,
            }
        };
        match next {
            Some(info) => deliver(t, info),
            None => return,
        }
    }
}

/// Symbolize a guest address as library+offset using recorded file mappings.
pub fn where_is(t: &Thread, pc: u64) -> String {
    let maps = t.proc.file_maps.lock().unwrap();
    maps.iter()
        .find(|(st, len, _, _)| pc >= *st && pc < st + len)
        .map(|(st, _, off, path)| format!("{}+{:#x}", path.rsplit('/').next().unwrap_or(path), pc - st + off))
        .unwrap_or_else(|| format!("{pc:#x}"))
}

/// Frame-pointer backtrace of the guest thread (AAPCS64 frame records).
pub fn backtrace(t: &Thread) {
    eprintln!("[wwd] guest backtrace (tid {}):", t.tid);
    eprintln!("    #00 pc {}", where_is(t, t.cpu.pc));
    eprintln!("    #01 lr {}", where_is(t, t.cpu.x[30]));
    let mut fp = crate::mem::untag(t.cpu.x[29]);
    for i in 2..48 {
        if fp == 0 || fp & 7 != 0 || !crate::mem::readable(fp, 16) {
            break;
        }
        let (next, lr): (u64, u64) = unsafe { (read_obj(fp), read_obj(fp + 8)) };
        if lr == 0 {
            break;
        }
        eprintln!("    #{i:02} {}", where_is(t, crate::mem::untag(lr)));
        if crate::mem::untag(next) <= fp {
            break;
        }
        fp = crate::mem::untag(next);
    }
}
