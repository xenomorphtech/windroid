//! Reference interpreter. Every instruction the decoder understands is
//! implemented here; the JIT handles a hot subset natively and calls back
//! into `exec` for everything else, so this file is the single source of
//! truth for guest semantics.
//!
//! Guest memory is identity-mapped: a guest virtual address *is* a host
//! pointer. Faults surface as host access violations and are reported by the
//! runtime's exception handler.

use crate::cpu::*;
use crate::decode::*;
use crate::simd;
use std::sync::atomic::*;

// ---------------------------------------------------------------------------
// memory helpers
// ---------------------------------------------------------------------------

/// Top-Byte-Ignore: strip a pointer tag before touching host memory.
#[inline(always)]
pub fn untag(addr: u64) -> u64 {
    addr & 0x00ff_ffff_ffff_ffff
}

#[inline(always)]
pub unsafe fn load(addr: u64, size: u8) -> u64 {
    let p = untag(addr) as *const u8;
    match size {
        1 => *p as u64,
        2 => (p as *const u16).read_unaligned() as u64,
        4 => (p as *const u32).read_unaligned() as u64,
        _ => (p as *const u64).read_unaligned(),
    }
}

#[inline(always)]
pub unsafe fn store(addr: u64, size: u8, v: u64) {
    let p = untag(addr) as *mut u8;
    match size {
        1 => *p = v as u8,
        2 => (p as *mut u16).write_unaligned(v as u16),
        4 => (p as *mut u32).write_unaligned(v as u32),
        _ => (p as *mut u64).write_unaligned(v),
    }
}

#[inline(always)]
pub unsafe fn load128(addr: u64) -> u128 {
    (untag(addr) as *const u128).read_unaligned()
}

#[inline(always)]
pub unsafe fn store128(addr: u64, v: u128) {
    (untag(addr) as *mut u128).write_unaligned(v)
}

#[inline(always)]
fn mask(v: u64, sf: bool) -> u64 {
    if sf {
        v
    } else {
        v & 0xffff_ffff
    }
}

#[inline(always)]
fn ones(n: u32) -> u64 {
    if n >= 64 {
        u64::MAX
    } else {
        (1u64 << n) - 1
    }
}

#[inline(always)]
fn sext_bits(v: u64, width: u32) -> u64 {
    if width >= 64 {
        return v;
    }
    let s = 64 - width;
    (((v << s) as i64) >> s) as u64
}

pub fn shift_reg(v: u64, sh: Shift, amt: u8, sf: bool) -> u64 {
    let a = amt as u32;
    if sf {
        match sh {
            Shift::Lsl => v << a,
            Shift::Lsr => v >> a,
            Shift::Asr => ((v as i64) >> a) as u64,
            Shift::Ror => v.rotate_right(a),
        }
    } else {
        let w = v as u32;
        (match sh {
            Shift::Lsl => w << a,
            Shift::Lsr => w >> a,
            Shift::Asr => ((w as i32) >> a) as u32,
            Shift::Ror => w.rotate_right(a),
        }) as u64
    }
}

pub fn extend_reg(v: u64, ext: Extend, shift: u8, sf: bool) -> u64 {
    let e = match ext {
        Extend::Uxtb => v & 0xff,
        Extend::Uxth => v & 0xffff,
        Extend::Uxtw => v & 0xffff_ffff,
        Extend::Uxtx => v,
        Extend::Sxtb => v as i8 as i64 as u64,
        Extend::Sxth => v as i16 as i64 as u64,
        Extend::Sxtw => v as i32 as i64 as u64,
        Extend::Sxtx => v,
    };
    mask(e << shift, sf)
}

fn bitfield(cpu: &Cpu, sf: bool, op: BitfieldOp, rd: u8, rn: u8, immr: u8, imms: u8) -> u64 {
    let ds: u32 = if sf { 64 } else { 32 };
    let src = cpu.rs(rn, sf);
    let r = immr as u32;
    let s = imms as u32;
    let res = match op {
        BitfieldOp::Ubfm => {
            if s >= r {
                (src >> r) & ones(s - r + 1)
            } else {
                (src & ones(s + 1)) << (ds - r)
            }
        }
        BitfieldOp::Sbfm => {
            if s >= r {
                let w = s - r + 1;
                sext_bits((src >> r) & ones(w), w)
            } else {
                let w = s + 1;
                sext_bits(src & ones(w), w) << (ds - r)
            }
        }
        BitfieldOp::Bfm => {
            let dst = cpu.rs(rd, sf);
            if s >= r {
                let w = s - r + 1;
                (dst & !ones(w)) | ((src >> r) & ones(w))
            } else {
                let w = s + 1;
                let pos = ds - r;
                (dst & !(ones(w) << pos)) | ((src & ones(w)) << pos)
            }
        }
    };
    mask(res, sf)
}

fn crc32(acc: u32, val: u64, size: u8, castagnoli: bool) -> u32 {
    let poly: u32 = if castagnoli { 0x82F6_3B78 } else { 0xEDB8_8320 };
    let mut crc = acc;
    for i in 0..size {
        crc ^= ((val >> (i * 8)) & 0xff) as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ poly } else { crc >> 1 };
        }
    }
    crc
}

fn ticks() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// Frequency reported via CNTFRQ_EL0. CNTVCT_EL0 counts nanoseconds.
pub const COUNTER_FREQ: u64 = 1_000_000_000;

fn read_sysreg(cpu: &Cpu, reg: u32) -> Option<u64> {
    use sysregs::*;
    Some(match reg {
        NZCV => cpu.nzcv << 28,
        FPCR => cpu.fpcr,
        FPSR => cpu.fpsr,
        TPIDR_EL0 => cpu.tpidr,
        TPIDRRO_EL0 => 0,
        // 64-byte I/D cache lines, IDC/DIC clear
        CTR_EL0 => 0x8444_c004,
        // DC ZVA prohibited: libc will use plain stores instead
        DCZID_EL0 => 0x10,
        MIDR_EL1 => 0x410f_d0c0, // "ARM Cortex-A76"
        MPIDR_EL1 => 0x8000_0000,
        REVIDR_EL1 => 0,
        CNTFRQ_EL0 => COUNTER_FREQ,
        CNTVCT_EL0 | CNTVCTSS_EL0 => ticks(),
        // Atomic = 2 (LSE), CRC32 = 1
        ID_AA64ISAR0_EL1 => (2 << 20) | (1 << 16),
        ID_AA64ISAR1_EL1 => 0,
        // EL0 AArch64 only, FP = 0, AdvSIMD = 0 (implemented)
        ID_AA64PFR0_EL1 => 0x0000_0000_0000_0011,
        ID_AA64PFR1_EL1 => 0,
        ID_AA64MMFR0_EL1 => 0x0000_0000_0000_0005,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// 128-bit compare-exchange (for LDXP/STXP of X registers and CASP X)
// ---------------------------------------------------------------------------

/// Returns the previous value; success iff it equals `expected`.
unsafe fn cmpxchg16b(addr: u64, expected: u128, new: u128) -> u128 {
    let addr = untag(addr);
    let (mut lo, mut hi) = (expected as u64, (expected >> 64) as u64);
    let new_lo = new as u64;
    let new_hi = (new >> 64) as u64;
    // rbx is reserved by LLVM, so swap it in and out manually.
    core::arch::asm!(
        "xchg {nlo}, rbx",
        "lock cmpxchg16b [{p}]",
        "mov rbx, {nlo}",
        p = in(reg) addr,
        nlo = inout(reg) new_lo => _,
        in("rcx") new_hi,
        inout("rax") lo,
        inout("rdx") hi,
        options(nostack)
    );
    (lo as u128) | ((hi as u128) << 64)
}

unsafe fn cmpxchg(addr: u64, size: u8, expected: u64, new: u64) -> u64 {
    let addr = untag(addr);
    match size {
        1 => match AtomicU8::from_ptr(addr as *mut u8).compare_exchange(expected as u8, new as u8, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(v) | Err(v) => v as u64,
        },
        2 => match AtomicU16::from_ptr(addr as *mut u16).compare_exchange(expected as u16, new as u16, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(v) | Err(v) => v as u64,
        },
        4 => match AtomicU32::from_ptr(addr as *mut u32).compare_exchange(expected as u32, new as u32, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(v) | Err(v) => v as u64,
        },
        _ => match AtomicU64::from_ptr(addr as *mut u64).compare_exchange(expected, new, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(v) | Err(v) => v,
        },
    }
}

macro_rules! atomic_rmw {
    ($addr:expr, $val:expr, $op:expr, $u:ty, $i:ty, $au:ty, $ai:ty) => {{
        let pu = <$au>::from_ptr($addr as *mut $u);
        let pi = <$ai>::from_ptr($addr as *mut $i);
        let v = $val;
        let o = Ordering::SeqCst;
        (match $op {
            AtomicOp::Add => pu.fetch_add(v as $u, o),
            AtomicOp::Clr => pu.fetch_and(!(v as $u), o),
            AtomicOp::Eor => pu.fetch_xor(v as $u, o),
            AtomicOp::Set => pu.fetch_or(v as $u, o),
            AtomicOp::Swp => pu.swap(v as $u, o),
            AtomicOp::Umax => pu.fetch_max(v as $u, o),
            AtomicOp::Umin => pu.fetch_min(v as $u, o),
            AtomicOp::Smax => pi.fetch_max(v as $i, o) as $u,
            AtomicOp::Smin => pi.fetch_min(v as $i, o) as $u,
        }) as u64
    }};
}

unsafe fn atomic(addr: u64, size: u8, op: AtomicOp, v: u64) -> u64 {
    let addr = untag(addr);
    match size {
        1 => atomic_rmw!(addr, v, op, u8, i8, AtomicU8, AtomicI8),
        2 => atomic_rmw!(addr, v, op, u16, i16, AtomicU16, AtomicI16),
        4 => atomic_rmw!(addr, v, op, u32, i32, AtomicU32, AtomicI32),
        _ => atomic_rmw!(addr, v, op, u64, i64, AtomicU64, AtomicI64),
    }
}

// ---------------------------------------------------------------------------
// execution
// ---------------------------------------------------------------------------

/// Signal the dispatcher. Returns false so callers can `return exit(...)`.
#[inline(always)]
pub fn exit(cpu: &mut Cpu, reason: ExitReason, arg: u64) -> bool {
    cpu.exit_reason = reason as u32;
    cpu.exit_arg = arg;
    false
}

fn mem_addr(cpu: &Cpu, rn: u8, mode: AddrMode) -> (u64, Option<u64>) {
    let base = cpu.r(rn);
    match mode {
        AddrMode::Offset(i) => (base.wrapping_add(i as u64), None),
        AddrMode::Pre(i) => {
            let a = base.wrapping_add(i as u64);
            (a, Some(a))
        }
        AddrMode::Post(i) => (base, Some(base.wrapping_add(i as u64))),
        AddrMode::Reg { rm, ext, shift } => (base.wrapping_add(extend_reg(cpu.r(rm), ext, shift, true)), None),
    }
}

#[inline(always)]
fn load_value(op: MemOp, raw: u64) -> u64 {
    if op.signed {
        let v = sext_bits(raw, op.size as u32 * 8);
        if op.dst64 {
            v
        } else {
            v & 0xffff_ffff
        }
    } else {
        raw
    }
}

unsafe fn do_load(cpu: &mut Cpu, op: MemOp, rt: u8, addr: u64) {
    if op.prefetch {
        return;
    }
    if op.fp {
        cpu.v[rt as usize] = if op.size == 16 { load128(addr) } else { load(addr, op.size) as u128 };
    } else {
        let v = load_value(op, load(addr, op.size));
        cpu.set(rt, v);
    }
}

unsafe fn do_store(cpu: &Cpu, op: MemOp, rt: u8, addr: u64) {
    if op.fp {
        if op.size == 16 {
            store128(addr, cpu.v[rt as usize]);
        } else {
            store(addr, op.size, cpu.v[rt as usize] as u64);
        }
    } else {
        store(addr, op.size, cpu.r(rt));
    }
}

/// Execute one decoded instruction located at `cpu.pc`.
///
/// On return `cpu.pc` holds the next PC. Returns `true` to keep going, or
/// `false` if the dispatcher must handle `cpu.exit_reason`.
pub fn exec(cpu: &mut Cpu, insn: Insn) -> bool {
    let pc = cpu.pc;
    let next = pc.wrapping_add(4);
    unsafe {
        match insn {
            Insn::Adr { rd, value } => cpu.set(rd, value),
            Insn::AddSubImm { sf, sub, s, rd, rn, imm } => {
                let a = cpu.rs(rn, sf);
                let (r, f) = if sub { add_with_carry(a, !imm, true, sf) } else { add_with_carry(a, imm, false, sf) };
                if s {
                    cpu.nzcv = f;
                }
                cpu.sets(rd, r, sf);
            }
            Insn::LogicImm { sf, op, rd, rn, imm } => {
                let a = cpu.rs(rn, sf);
                let r = match op {
                    LogicOp::And | LogicOp::Ands => a & imm,
                    LogicOp::Orr => a | imm,
                    LogicOp::Eor => a ^ imm,
                };
                if op == LogicOp::Ands {
                    cpu.nzcv = logic_flags(r, sf);
                }
                cpu.sets(rd, r, sf);
            }
            Insn::MovWide { sf, op, rd, imm, shift } => {
                let v = imm << shift;
                let r = match op {
                    MovOp::N => !v,
                    MovOp::Z => v,
                    MovOp::K => (cpu.r(rd) & !(0xffffu64 << shift)) | v,
                };
                cpu.sets(rd, r, sf);
            }
            Insn::Bitfield { sf, op, rd, rn, immr, imms } => {
                let r = bitfield(cpu, sf, op, rd, rn, immr, imms);
                cpu.set(rd, r);
            }
            Insn::Extr { sf, rd, rn, rm, lsb } => {
                let l = lsb as u32;
                let r = if sf {
                    if l == 0 {
                        cpu.r(rm)
                    } else {
                        (cpu.r(rm) >> l) | (cpu.r(rn) << (64 - l))
                    }
                } else {
                    let m = cpu.r(rm) as u32;
                    let n = cpu.r(rn) as u32;
                    (if l == 0 { m } else { (m >> l) | (n << (32 - l)) }) as u64
                };
                cpu.set(rd, r);
            }

            Insn::B { target, link } => {
                if link {
                    cpu.set(30, next);
                }
                cpu.pc = target;
                return true;
            }
            Insn::BCond { cond, target } => {
                cpu.pc = if cond_holds(cond, cpu.nzcv) { target } else { next };
                return true;
            }
            Insn::Cbz { sf, nz, rt, target } => {
                let z = cpu.rs(rt, sf) == 0;
                cpu.pc = if z != nz { target } else { next };
                return true;
            }
            Insn::Tbz { nz, rt, bit, target } => {
                let set = (cpu.r(rt) >> bit) & 1 != 0;
                cpu.pc = if set == nz { target } else { next };
                return true;
            }
            Insn::BrReg { rn, link } => {
                let t = cpu.r(rn);
                if link {
                    cpu.set(30, next);
                }
                cpu.pc = t;
                return true;
            }
            Insn::Svc { imm } => {
                cpu.pc = next;
                return exit(cpu, ExitReason::Svc, imm as u64);
            }
            Insn::Brk { imm } => return exit(cpu, ExitReason::Brk, imm as u64),
            Insn::Udf { imm } => return exit(cpu, ExitReason::Undefined, imm as u64),
            Insn::Hint | Insn::MsrImm => {}
            Insn::Barrier { full } => {
                if full {
                    std::sync::atomic::fence(Ordering::SeqCst);
                }
            }
            Insn::Clrex => cpu.excl_addr = u64::MAX,
            Insn::Mrs { rt, reg } => match read_sysreg(cpu, reg) {
                Some(v) => cpu.set(rt, v),
                None => return exit(cpu, ExitReason::Undefined, reg as u64),
            },
            Insn::Msr { rt, reg } => {
                let v = cpu.r(rt);
                match reg {
                    sysregs::NZCV => cpu.nzcv = (v >> 28) & 15,
                    sysregs::FPCR => cpu.fpcr = v,
                    sysregs::FPSR => cpu.fpsr = v,
                    sysregs::TPIDR_EL0 => cpu.tpidr = v,
                    _ => return exit(cpu, ExitReason::Undefined, reg as u64),
                }
            }
            Insn::Sys { op1, crn, crm, op2, rt } => {
                match (op1, crn, crm, op2) {
                    (3, 7, 4, 1) => {
                        // DC ZVA
                        let a = untag(cpu.r(rt)) & !63;
                        std::ptr::write_bytes(a as *mut u8, 0, 64);
                    }
                    (3, 7, 5, 1) => {
                        // IC IVAU: guest modified code; our translations may be stale
                        cpu.pc = next;
                        return exit(cpu, ExitReason::CacheFlush, cpu.r(rt));
                    }
                    _ => {} // other cache maintenance: no-op on a coherent host
                }
            }

            Insn::LogicReg { sf, op, invert, shift, amount, rd, rn, rm } => {
                let mut b = shift_reg(cpu.rs(rm, sf), shift, amount, sf);
                if invert {
                    b = mask(!b, sf);
                }
                let a = cpu.rs(rn, sf);
                let r = match op {
                    LogicOp::And | LogicOp::Ands => a & b,
                    LogicOp::Orr => a | b,
                    LogicOp::Eor => a ^ b,
                };
                if op == LogicOp::Ands {
                    cpu.nzcv = logic_flags(r, sf);
                }
                cpu.sets(rd, r, sf);
            }
            Insn::AddSubShift { sf, sub, s, shift, amount, rd, rn, rm } => {
                let b = shift_reg(cpu.rs(rm, sf), shift, amount, sf);
                let a = cpu.rs(rn, sf);
                let (r, f) = if sub { add_with_carry(a, !b, true, sf) } else { add_with_carry(a, b, false, sf) };
                if s {
                    cpu.nzcv = f;
                }
                cpu.sets(rd, r, sf);
            }
            Insn::AddSubExt { sf, sub, s, ext, amount, rd, rn, rm } => {
                let b = extend_reg(cpu.r(rm), ext, amount, sf);
                let a = cpu.rs(rn, sf);
                let (r, f) = if sub { add_with_carry(a, !b, true, sf) } else { add_with_carry(a, b, false, sf) };
                if s {
                    cpu.nzcv = f;
                }
                cpu.sets(rd, r, sf);
            }
            Insn::Adc { sf, sub, s, rd, rn, rm } => {
                let a = cpu.rs(rn, sf);
                let b = cpu.rs(rm, sf);
                let c = cpu.nzcv & 2 != 0;
                let (r, f) = add_with_carry(a, if sub { !b } else { b }, c, sf);
                if s {
                    cpu.nzcv = f;
                }
                cpu.sets(rd, r, sf);
            }
            Insn::CondCmp { sf, sub, rn, rm, imm, cond, nzcv } => {
                if cond_holds(cond, cpu.nzcv) {
                    let a = cpu.rs(rn, sf);
                    let b = match imm {
                        Some(i) => i as u64,
                        None => cpu.rs(rm, sf),
                    };
                    let (_, f) = if sub { add_with_carry(a, !b, true, sf) } else { add_with_carry(a, b, false, sf) };
                    cpu.nzcv = f;
                } else {
                    cpu.nzcv = nzcv as u64;
                }
            }
            Insn::CondSel { sf, op, rd, rn, rm, cond } => {
                let r = if cond_holds(cond, cpu.nzcv) {
                    cpu.rs(rn, sf)
                } else {
                    let m = cpu.rs(rm, sf);
                    match op {
                        CselOp::Csel => m,
                        CselOp::Csinc => m.wrapping_add(1),
                        CselOp::Csinv => !m,
                        CselOp::Csneg => m.wrapping_neg(),
                    }
                };
                cpu.sets(rd, r, sf);
            }
            Insn::Dp1 { sf, op, rd, rn } => {
                let v = cpu.rs(rn, sf);
                let r = if sf {
                    match op {
                        Dp1Op::Rbit => v.reverse_bits(),
                        Dp1Op::Rev16 => ((v & 0x00ff_00ff_00ff_00ff) << 8) | ((v >> 8) & 0x00ff_00ff_00ff_00ff),
                        Dp1Op::Rev32 => ((v as u32).swap_bytes() as u64) | ((((v >> 32) as u32).swap_bytes() as u64) << 32),
                        Dp1Op::Rev => v.swap_bytes(),
                        Dp1Op::Clz => v.leading_zeros() as u64,
                        Dp1Op::Cls => (v ^ (((v as i64) >> 63) as u64)).leading_zeros() as u64 - 1,
                        Dp1Op::Ctz => v.trailing_zeros() as u64,
                        Dp1Op::Cnt => v.count_ones() as u64,
                        Dp1Op::Abs => (v as i64).wrapping_abs() as u64,
                    }
                } else {
                    let w = v as u32;
                    (match op {
                        Dp1Op::Rbit => w.reverse_bits(),
                        Dp1Op::Rev16 => ((w & 0x00ff_00ff) << 8) | ((w >> 8) & 0x00ff_00ff),
                        Dp1Op::Rev32 | Dp1Op::Rev => w.swap_bytes(),
                        Dp1Op::Clz => w.leading_zeros(),
                        Dp1Op::Cls => (w ^ (((w as i32) >> 31) as u32)).leading_zeros() - 1,
                        Dp1Op::Ctz => w.trailing_zeros(),
                        Dp1Op::Cnt => w.count_ones(),
                        Dp1Op::Abs => (w as i32).wrapping_abs() as u32,
                    }) as u64
                };
                cpu.set(rd, r);
            }
            Insn::Dp2 { sf, op, rd, rn, rm } => {
                let a = cpu.rs(rn, sf);
                let b = cpu.rs(rm, sf);
                let ds = if sf { 64 } else { 32 };
                let r = match op {
                    Dp2Op::Udiv => {
                        if b == 0 {
                            0
                        } else {
                            a / b
                        }
                    }
                    Dp2Op::Sdiv => {
                        if b == 0 {
                            0
                        } else if sf {
                            (a as i64).wrapping_div(b as i64) as u64
                        } else {
                            (a as u32 as i32).wrapping_div(b as u32 as i32) as u32 as u64
                        }
                    }
                    Dp2Op::Lslv => shift_reg(a, Shift::Lsl, (b % ds) as u8, sf),
                    Dp2Op::Lsrv => shift_reg(a, Shift::Lsr, (b % ds) as u8, sf),
                    Dp2Op::Asrv => shift_reg(a, Shift::Asr, (b % ds) as u8, sf),
                    Dp2Op::Rorv => shift_reg(a, Shift::Ror, (b % ds) as u8, sf),
                    Dp2Op::Crc32 { size, c } => crc32(a as u32, cpu.r(rm), size, c) as u64,
                    Dp2Op::Smax => {
                        if sf {
                            (a as i64).max(b as i64) as u64
                        } else {
                            (a as i32).max(b as i32) as u32 as u64
                        }
                    }
                    Dp2Op::Smin => {
                        if sf {
                            (a as i64).min(b as i64) as u64
                        } else {
                            (a as i32).min(b as i32) as u32 as u64
                        }
                    }
                    Dp2Op::Umax => a.max(b),
                    Dp2Op::Umin => a.min(b),
                };
                cpu.sets(rd, r, sf);
            }
            Insn::Dp3 { sf, op, rd, rn, rm, ra } => {
                let n = cpu.r(rn);
                let m = cpu.r(rm);
                let acc = cpu.r(ra);
                let r = match op {
                    Dp3Op::Madd => acc.wrapping_add(n.wrapping_mul(m)),
                    Dp3Op::Msub => acc.wrapping_sub(n.wrapping_mul(m)),
                    Dp3Op::Smaddl => acc.wrapping_add(((n as i32 as i64) * (m as i32 as i64)) as u64),
                    Dp3Op::Smsubl => acc.wrapping_sub(((n as i32 as i64) * (m as i32 as i64)) as u64),
                    Dp3Op::Umaddl => acc.wrapping_add((n as u32 as u64) * (m as u32 as u64)),
                    Dp3Op::Umsubl => acc.wrapping_sub((n as u32 as u64) * (m as u32 as u64)),
                    Dp3Op::Smulh => (((n as i64 as i128) * (m as i64 as i128)) >> 64) as u64,
                    Dp3Op::Umulh => (((n as u128) * (m as u128)) >> 64) as u64,
                };
                cpu.sets(rd, r, sf);
            }

            Insn::LdSt { op, rt, rn, mode } => {
                let (addr, wb) = mem_addr(cpu, rn, mode);
                if op.load {
                    do_load(cpu, op, rt, addr);
                } else {
                    do_store(cpu, op, rt, addr);
                }
                if let Some(w) = wb {
                    cpu.set(rn, w);
                }
            }
            Insn::LdStPair { op, rt, rt2, rn, mode } => {
                let (addr, wb) = mem_addr(cpu, rn, mode);
                let a2 = addr.wrapping_add(op.size as u64);
                if op.load {
                    if op.fp {
                        let (v1, v2) = if op.size == 16 {
                            (load128(addr), load128(a2))
                        } else {
                            (load(addr, op.size) as u128, load(a2, op.size) as u128)
                        };
                        cpu.v[rt as usize] = v1;
                        cpu.v[rt2 as usize] = v2;
                    } else {
                        let v1 = load_value(op, load(addr, op.size));
                        let v2 = load_value(op, load(a2, op.size));
                        cpu.set(rt, v1);
                        cpu.set(rt2, v2);
                    }
                } else {
                    do_store(cpu, op, rt, addr);
                    do_store(cpu, op, rt2, a2);
                }
                if let Some(w) = wb {
                    cpu.set(rn, w);
                }
            }
            Insn::LdLit { op, rt, addr } => do_load(cpu, op, rt, addr),

            Insn::Ldx { size, pair, rt, rt2, rn } => {
                let addr = cpu.r(rn);
                if pair {
                    if size == 8 {
                        let v = load128(addr);
                        cpu.excl_val = v as u64;
                        cpu.excl_val2 = (v >> 64) as u64;
                        cpu.set(rt, v as u64);
                        cpu.set(rt2, (v >> 64) as u64);
                    } else {
                        let v = load(addr, 8);
                        cpu.excl_val = v;
                        cpu.set(rt, v & 0xffff_ffff);
                        cpu.set(rt2, v >> 32);
                    }
                } else {
                    let v = load(addr, size);
                    cpu.excl_val = v;
                    cpu.set(rt, v);
                }
                cpu.excl_addr = addr;
            }
            Insn::Stx { size, pair, rs, rt, rt2, rn } => {
                let addr = cpu.r(rn);
                let ok = if cpu.excl_addr != addr {
                    false
                } else if pair {
                    if size == 8 {
                        let exp = (cpu.excl_val as u128) | ((cpu.excl_val2 as u128) << 64);
                        let new = (cpu.r(rt) as u128) | ((cpu.r(rt2) as u128) << 64);
                        cmpxchg16b(addr, exp, new) == exp
                    } else {
                        let new = (cpu.r(rt) & 0xffff_ffff) | (cpu.r(rt2) << 32);
                        cmpxchg(addr, 8, cpu.excl_val, new) == cpu.excl_val
                    }
                } else {
                    let m = if size == 8 { u64::MAX } else { (1u64 << (size * 8)) - 1 };
                    cmpxchg(addr, size, cpu.excl_val, cpu.r(rt)) & m == cpu.excl_val & m
                };
                cpu.excl_addr = u64::MAX;
                cpu.set(rs, if ok { 0 } else { 1 });
            }
            Insn::Ldar { size, rt, rn } => {
                let v = load(cpu.r(rn), size);
                cpu.set(rt, v);
            }
            Insn::Stlr { size, rt, rn } => {
                store(cpu.r(rn), size, cpu.r(rt));
                std::sync::atomic::fence(Ordering::SeqCst);
            }
            Insn::Cas { size, rs, rt, rn } => {
                let m = if size == 8 { u64::MAX } else { (1u64 << (size * 8)) - 1 };
                let old = cmpxchg(cpu.r(rn), size, cpu.r(rs) & m, cpu.r(rt) & m);
                cpu.set(rs, old);
            }
            Insn::Casp { size, rs, rt, rn } => {
                let addr = cpu.r(rn);
                if size == 8 {
                    let exp = (cpu.r(rs) as u128) | ((cpu.r(rs + 1) as u128) << 64);
                    let new = (cpu.r(rt) as u128) | ((cpu.r(rt + 1) as u128) << 64);
                    let old = cmpxchg16b(addr, exp, new);
                    cpu.set(rs, old as u64);
                    cpu.set(rs + 1, (old >> 64) as u64);
                } else {
                    let exp = (cpu.r(rs) & 0xffff_ffff) | (cpu.r(rs + 1) << 32);
                    let new = (cpu.r(rt) & 0xffff_ffff) | (cpu.r(rt + 1) << 32);
                    let old = cmpxchg(addr, 8, exp, new);
                    cpu.set(rs, old & 0xffff_ffff);
                    cpu.set(rs + 1, old >> 32);
                }
            }
            Insn::Atomic { size, op, rs, rt, rn } => {
                let old = atomic(cpu.r(rn), size, op, cpu.r(rs));
                cpu.set(rt, old);
            }

            Insn::Simd(w) => {
                if !simd::exec(cpu, w) {
                    return exit(cpu, ExitReason::Undefined, w as u64);
                }
            }
            Insn::SimdLdSt(w) => {
                if !simd::exec_ldst(cpu, w) {
                    return exit(cpu, ExitReason::Undefined, w as u64);
                }
            }
            Insn::Unknown(w) => return exit(cpu, ExitReason::Undefined, w as u64),
        }
    }
    cpu.pc = next;
    true
}

/// Fetch, decode and execute the instruction at `cpu.pc`.
pub fn step(cpu: &mut Cpu) -> bool {
    let w = unsafe { (cpu.pc as *const u32).read_unaligned() };
    exec(cpu, decode(w, cpu.pc))
}
