//! A64 (ARMv8-A AArch64) instruction decoder.
//!
//! Register operand convention used throughout the CPU crate:
//!   0..=30  -> X0..X30
//!   31      -> XZR/WZR (reads as zero, writes discarded)
//!   32      -> SP
//! The decoder resolves the "31 means SP or ZR" ambiguity per encoding, so
//! consumers never need to care.
//!
//! PC-relative operands are resolved to absolute addresses at decode time,
//! which is why `decode` takes the instruction's address.

pub const ZR: u8 = 31;
pub const SP: u8 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shift {
    Lsl,
    Lsr,
    Asr,
    Ror,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extend {
    Uxtb,
    Uxth,
    Uxtw,
    Uxtx,
    Sxtb,
    Sxth,
    Sxtw,
    Sxtx,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicOp {
    And,
    Orr,
    Eor,
    Ands,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitfieldOp {
    Sbfm,
    Bfm,
    Ubfm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovOp {
    N,
    Z,
    K,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CselOp {
    Csel,
    Csinc,
    Csinv,
    Csneg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dp1Op {
    Rbit,
    /// Byte-reverse each halfword.
    Rev16,
    /// Byte-reverse each word (64-bit only).
    Rev32,
    /// Byte-reverse the whole register (REV / REV64 for X).
    Rev,
    Clz,
    Cls,
    Ctz,
    Cnt,
    Abs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dp2Op {
    Udiv,
    Sdiv,
    Lslv,
    Lsrv,
    Asrv,
    Rorv,
    Crc32 { size: u8, c: bool },
    Smax,
    Smin,
    Umax,
    Umin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dp3Op {
    Madd,
    Msub,
    Smaddl,
    Smsubl,
    Umaddl,
    Umsubl,
    Smulh,
    Umulh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomicOp {
    Add,
    Clr,
    Eor,
    Set,
    Smax,
    Smin,
    Umax,
    Umin,
    Swp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddrMode {
    /// [Rn, #imm]
    Offset(i64),
    /// [Rn, #imm]!
    Pre(i64),
    /// [Rn], #imm
    Post(i64),
    /// [Rn, Rm{, ext #shift}]
    Reg { rm: u8, ext: Extend, shift: u8 },
}

/// Shape of a single-register memory access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemOp {
    /// Access size in bytes: 1, 2, 4, 8 or 16.
    pub size: u8,
    pub load: bool,
    /// Sign-extend the loaded value.
    pub signed: bool,
    /// Destination is a 64-bit X register (only meaningful for signed loads).
    pub dst64: bool,
    /// Rt names a SIMD&FP register.
    pub fp: bool,
    /// PRFM: no architectural effect.
    pub prefetch: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Insn {
    // ---- data processing, immediate ----
    /// ADR/ADRP with the target already resolved.
    Adr { rd: u8, value: u64 },
    AddSubImm { sf: bool, sub: bool, s: bool, rd: u8, rn: u8, imm: u64 },
    LogicImm { sf: bool, op: LogicOp, rd: u8, rn: u8, imm: u64 },
    MovWide { sf: bool, op: MovOp, rd: u8, imm: u64, shift: u8 },
    Bitfield { sf: bool, op: BitfieldOp, rd: u8, rn: u8, immr: u8, imms: u8 },
    Extr { sf: bool, rd: u8, rn: u8, rm: u8, lsb: u8 },

    // ---- branches / exceptions / system ----
    B { target: u64, link: bool },
    BCond { cond: u8, target: u64 },
    Cbz { sf: bool, nz: bool, rt: u8, target: u64 },
    Tbz { nz: bool, rt: u8, bit: u8, target: u64 },
    /// BR / BLR / RET (and their pointer-auth variants, which we treat as plain).
    BrReg { rn: u8, link: bool },
    Svc { imm: u16 },
    Brk { imm: u16 },
    Udf { imm: u16 },
    /// NOP, YIELD, BTI, PACIASP, ... — no effect for us.
    Hint,
    Barrier { full: bool },
    Clrex,
    Mrs { rt: u8, reg: u32 },
    Msr { rt: u8, reg: u32 },
    /// MSR (immediate) to PSTATE fields — ignored.
    MsrImm,
    /// SYS instructions (DC, IC, AT, TLBI).
    Sys { op1: u8, crn: u8, crm: u8, op2: u8, rt: u8 },

    // ---- data processing, register ----
    LogicReg { sf: bool, op: LogicOp, invert: bool, shift: Shift, amount: u8, rd: u8, rn: u8, rm: u8 },
    AddSubShift { sf: bool, sub: bool, s: bool, shift: Shift, amount: u8, rd: u8, rn: u8, rm: u8 },
    AddSubExt { sf: bool, sub: bool, s: bool, ext: Extend, amount: u8, rd: u8, rn: u8, rm: u8 },
    Adc { sf: bool, sub: bool, s: bool, rd: u8, rn: u8, rm: u8 },
    /// CCMP/CCMN. `rm` is ignored when `imm` is Some.
    CondCmp { sf: bool, sub: bool, rn: u8, rm: u8, imm: Option<u8>, cond: u8, nzcv: u8 },
    CondSel { sf: bool, op: CselOp, rd: u8, rn: u8, rm: u8, cond: u8 },
    Dp1 { sf: bool, op: Dp1Op, rd: u8, rn: u8 },
    Dp2 { sf: bool, op: Dp2Op, rd: u8, rn: u8, rm: u8 },
    Dp3 { sf: bool, op: Dp3Op, rd: u8, rn: u8, rm: u8, ra: u8 },

    // ---- loads and stores ----
    LdSt { op: MemOp, rt: u8, rn: u8, mode: AddrMode },
    LdStPair { op: MemOp, rt: u8, rt2: u8, rn: u8, mode: AddrMode },
    LdLit { op: MemOp, rt: u8, addr: u64 },
    /// LDXR/LDAXR/LDXP/LDAXP. `size` is the per-register size in bytes.
    Ldx { size: u8, pair: bool, rt: u8, rt2: u8, rn: u8 },
    /// STXR/STLXR/STXP/STLXP. `rs` receives the status (0 = success).
    Stx { size: u8, pair: bool, rs: u8, rt: u8, rt2: u8, rn: u8 },
    /// LDAR/LDAPR/LDLAR — plain loads with ordering (x86 TSO makes them free).
    Ldar { size: u8, rt: u8, rn: u8 },
    /// STLR/STLLR.
    Stlr { size: u8, rt: u8, rn: u8 },
    Cas { size: u8, rs: u8, rt: u8, rn: u8 },
    Casp { size: u8, rs: u8, rt: u8, rn: u8 },
    Atomic { size: u8, op: AtomicOp, rs: u8, rt: u8, rn: u8 },

    // ---- SIMD & FP (decoded/executed by the simd module) ----
    Simd(u32),
    /// Advanced SIMD structure loads/stores (LD1..LD4, ST1..ST4 and friends).
    SimdLdSt(u32),

    Unknown(u32),
}

impl Insn {
    /// Does this instruction (potentially) change control flow?
    pub fn is_branch(&self) -> bool {
        matches!(
            self,
            Insn::B { .. }
                | Insn::BCond { .. }
                | Insn::Cbz { .. }
                | Insn::Tbz { .. }
                | Insn::BrReg { .. }
                | Insn::Svc { .. }
                | Insn::Brk { .. }
                | Insn::Udf { .. }
                | Insn::Unknown(_)
        )
    }
}

#[inline(always)]
fn bits(w: u32, hi: u32, lo: u32) -> u32 {
    (w >> lo) & ((1u32 << (hi - lo + 1)) - 1)
}

#[inline(always)]
fn bit(w: u32, b: u32) -> bool {
    (w >> b) & 1 != 0
}

#[inline(always)]
fn sext(v: u64, width: u32) -> i64 {
    let s = 64 - width;
    ((v << s) as i64) >> s
}

#[inline(always)]
fn reg(w: u32, lo: u32) -> u8 {
    bits(w, lo + 4, lo) as u8
}

/// Register field where 31 encodes SP.
#[inline(always)]
fn reg_sp(w: u32, lo: u32) -> u8 {
    let r = reg(w, lo);
    if r == 31 {
        SP
    } else {
        r
    }
}

pub fn sysreg(op0: u32, op1: u32, crn: u32, crm: u32, op2: u32) -> u32 {
    (op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2
}

pub mod sysregs {
    use super::sysreg;
    pub const NZCV: u32 = 0xda10; // 3,3,4,2,0
    pub const FPCR: u32 = 0xda20; // 3,3,4,4,0
    pub const FPSR: u32 = 0xda21; // 3,3,4,4,1
    pub const TPIDR_EL0: u32 = 0xde82; // 3,3,13,0,2
    pub const TPIDRRO_EL0: u32 = 0xde83;
    pub const CTR_EL0: u32 = 0xd801; // 3,3,0,0,1
    pub const DCZID_EL0: u32 = 0xd807; // 3,3,0,0,7
    pub const MIDR_EL1: u32 = 0xc000; // 3,0,0,0,0
    pub const MPIDR_EL1: u32 = 0xc005;
    pub const REVIDR_EL1: u32 = 0xc006;
    pub const CNTFRQ_EL0: u32 = 0xdf00; // 3,3,14,0,0
    pub const CNTVCT_EL0: u32 = 0xdf02; // 3,3,14,0,2
    pub const CNTVCTSS_EL0: u32 = 0xdf06;
    pub const ID_AA64ISAR0_EL1: u32 = 0xc030;
    pub const ID_AA64ISAR1_EL1: u32 = 0xc031;
    pub const ID_AA64PFR0_EL1: u32 = 0xc020;
    pub const ID_AA64PFR1_EL1: u32 = 0xc021;
    pub const ID_AA64MMFR0_EL1: u32 = 0xc038;
    pub const RNDR: u32 = 0xd920; // 3,3,2,4,0
    // compile-time sanity checks on the packed constants
    const _: () = assert!(NZCV == sysreg_c(3, 3, 4, 2, 0));
    const _: () = assert!(TPIDR_EL0 == sysreg_c(3, 3, 13, 0, 2));
    const _: () = assert!(CNTVCT_EL0 == sysreg_c(3, 3, 14, 0, 2));
    const _: () = assert!(MIDR_EL1 == sysreg_c(3, 0, 0, 0, 0));
    const _: () = assert!(ID_AA64ISAR0_EL1 == sysreg_c(3, 0, 0, 6, 0));
    const _: () = assert!(ID_AA64PFR0_EL1 == sysreg_c(3, 0, 0, 4, 0));
    const _: () = assert!(ID_AA64MMFR0_EL1 == sysreg_c(3, 0, 0, 7, 0));
    const _: () = assert!(RNDR == sysreg_c(3, 3, 2, 4, 0));
    const fn sysreg_c(op0: u32, op1: u32, crn: u32, crm: u32, op2: u32) -> u32 {
        (op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2
    }
    #[allow(dead_code)]
    fn _use() -> u32 {
        sysreg(0, 0, 0, 0, 0)
    }
}

/// DecodeBitMasks() from the ARM ARM, returning the "wmask" immediate used by
/// the logical-immediate instructions.
pub fn decode_bit_masks(n: u32, imms: u32, immr: u32, sf: bool) -> Option<u64> {
    let combined = (n << 6) | (!imms & 0x3f);
    if combined == 0 {
        return None;
    }
    let len = 31 - combined.leading_zeros();
    if len < 1 || (!sf && n == 1) {
        return None;
    }
    let levels = (1u32 << len) - 1;
    let s = imms & levels;
    let r = immr & levels;
    if s == levels {
        return None;
    }
    let esize = 1u32 << len;
    let emask = if esize == 64 { u64::MAX } else { (1u64 << esize) - 1 };
    let welem = if s + 1 == 64 { u64::MAX } else { (1u64 << (s + 1)) - 1 };
    let elem = if r == 0 {
        welem
    } else {
        ((welem >> r) | (welem << (esize - r))) & emask
    };
    let mut v = elem;
    let mut e = esize;
    while e < 64 {
        v |= v << e;
        e *= 2;
    }
    Some(if sf { v } else { v & 0xffff_ffff })
}

fn shift_of(v: u32) -> Shift {
    match v {
        0 => Shift::Lsl,
        1 => Shift::Lsr,
        2 => Shift::Asr,
        _ => Shift::Ror,
    }
}

fn extend_of(v: u32) -> Extend {
    match v {
        0 => Extend::Uxtb,
        1 => Extend::Uxth,
        2 => Extend::Uxtw,
        3 => Extend::Uxtx,
        4 => Extend::Sxtb,
        5 => Extend::Sxth,
        6 => Extend::Sxtw,
        _ => Extend::Sxtx,
    }
}

pub fn decode(w: u32, pc: u64) -> Insn {
    let op0 = bits(w, 28, 25);
    match op0 {
        0b1000 | 0b1001 => decode_dp_imm(w, pc),
        0b1010 | 0b1011 => decode_branch_sys(w, pc),
        0b0100 | 0b0110 | 0b1100 | 0b1110 => decode_ldst(w, pc),
        0b0101 | 0b1101 => decode_dp_reg(w),
        0b0111 | 0b1111 => Insn::Simd(w),
        _ => {
            if bits(w, 31, 16) == 0 {
                Insn::Udf { imm: bits(w, 15, 0) as u16 }
            } else {
                Insn::Unknown(w)
            }
        }
    }
}

fn decode_dp_imm(w: u32, pc: u64) -> Insn {
    let sf = bit(w, 31);
    let rd = reg(w, 0);
    let rn = reg(w, 5);
    match bits(w, 25, 23) {
        0b000 | 0b001 => {
            let immlo = bits(w, 30, 29) as u64;
            let immhi = bits(w, 23, 5) as u64;
            let imm = sext((immhi << 2) | immlo, 21);
            let value = if bit(w, 31) {
                (pc & !0xfff).wrapping_add((imm << 12) as u64)
            } else {
                pc.wrapping_add(imm as u64)
            };
            Insn::Adr { rd, value }
        }
        0b010 => {
            let sub = bit(w, 30);
            let s = bit(w, 29);
            let mut imm = bits(w, 21, 10) as u64;
            if bit(w, 22) {
                imm <<= 12;
            }
            Insn::AddSubImm {
                sf,
                sub,
                s,
                rd: if s { rd } else { reg_sp(w, 0) },
                rn: reg_sp(w, 5),
                imm,
            }
        }
        0b100 => {
            let op = match bits(w, 30, 29) {
                0 => LogicOp::And,
                1 => LogicOp::Orr,
                2 => LogicOp::Eor,
                _ => LogicOp::Ands,
            };
            match decode_bit_masks(bits(w, 22, 22), bits(w, 15, 10), bits(w, 21, 16), sf) {
                Some(imm) => Insn::LogicImm {
                    sf,
                    op,
                    rd: if op == LogicOp::Ands { rd } else { reg_sp(w, 0) },
                    rn,
                    imm,
                },
                None => Insn::Unknown(w),
            }
        }
        0b101 => {
            let hw = bits(w, 22, 21);
            if !sf && hw >= 2 {
                return Insn::Unknown(w);
            }
            let op = match bits(w, 30, 29) {
                0 => MovOp::N,
                2 => MovOp::Z,
                3 => MovOp::K,
                _ => return Insn::Unknown(w),
            };
            Insn::MovWide { sf, op, rd, imm: bits(w, 20, 5) as u64, shift: (hw * 16) as u8 }
        }
        0b110 => {
            let op = match bits(w, 30, 29) {
                0 => BitfieldOp::Sbfm,
                1 => BitfieldOp::Bfm,
                2 => BitfieldOp::Ubfm,
                _ => return Insn::Unknown(w),
            };
            if bit(w, 22) != sf {
                return Insn::Unknown(w);
            }
            Insn::Bitfield { sf, op, rd, rn, immr: bits(w, 21, 16) as u8, imms: bits(w, 15, 10) as u8 }
        }
        0b111 => {
            if bits(w, 30, 29) != 0 || bit(w, 21) || bit(w, 22) != sf {
                return Insn::Unknown(w);
            }
            Insn::Extr { sf, rd, rn, rm: reg(w, 16), lsb: bits(w, 15, 10) as u8 }
        }
        _ => Insn::Unknown(w),
    }
}

fn decode_branch_sys(w: u32, pc: u64) -> Insn {
    // Unconditional branch (immediate)
    if bits(w, 30, 26) == 0b00101 {
        let imm = sext(bits(w, 25, 0) as u64, 26) << 2;
        return Insn::B { target: pc.wrapping_add(imm as u64), link: bit(w, 31) };
    }
    // Compare and branch
    if bits(w, 30, 25) == 0b011010 {
        let imm = sext(bits(w, 23, 5) as u64, 19) << 2;
        return Insn::Cbz { sf: bit(w, 31), nz: bit(w, 24), rt: reg(w, 0), target: pc.wrapping_add(imm as u64) };
    }
    // Test and branch
    if bits(w, 30, 25) == 0b011011 {
        let imm = sext(bits(w, 18, 5) as u64, 14) << 2;
        let b = (bits(w, 31, 31) << 5) | bits(w, 23, 19);
        return Insn::Tbz { nz: bit(w, 24), rt: reg(w, 0), bit: b as u8, target: pc.wrapping_add(imm as u64) };
    }
    // Conditional branch (B.cond and BC.cond)
    if bits(w, 31, 24) == 0b0101_0100 {
        let imm = sext(bits(w, 23, 5) as u64, 19) << 2;
        return Insn::BCond { cond: bits(w, 3, 0) as u8, target: pc.wrapping_add(imm as u64) };
    }
    // Exception generation
    if bits(w, 31, 24) == 0b1101_0100 {
        let opc = bits(w, 23, 21);
        let ll = bits(w, 1, 0);
        let imm = bits(w, 20, 5) as u16;
        return match (opc, bits(w, 4, 2), ll) {
            (0, 0, 1) => Insn::Svc { imm },
            (1, 0, 0) => Insn::Brk { imm },
            (2, 0, 0) => Insn::Brk { imm }, // HLT — treat like BRK
            _ => Insn::Unknown(w),
        };
    }
    // System
    if bits(w, 31, 22) == 0b11_0101_0100 {
        let l = bit(w, 21);
        let op0 = bits(w, 20, 19);
        let op1 = bits(w, 18, 16);
        let crn = bits(w, 15, 12);
        let crm = bits(w, 11, 8);
        let op2 = bits(w, 7, 5);
        let rt = reg(w, 0);
        if !l && op0 == 0 {
            if crn == 0b0010 && op1 == 0b011 && rt == 31 {
                return Insn::Hint;
            }
            if crn == 0b0011 && op1 == 0b011 && rt == 31 {
                return match op2 {
                    0b010 => Insn::Clrex,
                    // DMB/DSB with a full (load+store) domain need a real fence
                    // on x86 to order store->load; ISHLD/ISHST/ISB do not.
                    0b100 | 0b101 => Insn::Barrier { full: crm & 3 == 3 },
                    _ => Insn::Barrier { full: false },
                };
            }
            if crn == 0b0100 && rt == 31 {
                return Insn::MsrImm;
            }
            return Insn::Unknown(w);
        }
        if op0 == 1 {
            if l {
                return Insn::Unknown(w); // SYSL
            }
            return Insn::Sys { op1: op1 as u8, crn: crn as u8, crm: crm as u8, op2: op2 as u8, rt };
        }
        if op0 >= 2 {
            let r = sysreg(op0, op1, crn, crm, op2);
            return if l { Insn::Mrs { rt, reg: r } } else { Insn::Msr { rt, reg: r } };
        }
        return Insn::Unknown(w);
    }
    // Unconditional branch (register)
    if bits(w, 31, 25) == 0b110_1011 {
        let opc = bits(w, 24, 21);
        let op2 = bits(w, 20, 16);
        let op3 = bits(w, 15, 10);
        let rn = reg(w, 5);
        if op2 != 0b11111 {
            return Insn::Unknown(w);
        }
        // plain forms have op3 == 0; pointer-auth forms (BRAA, RETAA, ...) set op3
        // to 0b00001x. We do not implement PAC (HWCAP doesn't advertise it), but
        // compilers emit these in the hint-compatible forms anyway; treat as plain.
        let pac_ok = op3 == 0 || op3 == 0b000010 || op3 == 0b000011;
        if !pac_ok {
            return Insn::Unknown(w);
        }
        return match opc {
            0b0000 | 0b1000 => Insn::BrReg { rn, link: false }, // BR, BRAA
            0b0001 | 0b1001 => Insn::BrReg { rn, link: true },  // BLR, BLRAA
            0b0010 => Insn::BrReg { rn, link: false },          // RET, RETAA
            _ => Insn::Unknown(w),
        };
    }
    Insn::Unknown(w)
}

fn gp_memop(size: u32, opc: u32) -> Option<MemOp> {
    let bytes = 1u8 << size;
    let base = MemOp { size: bytes, load: false, signed: false, dst64: false, fp: false, prefetch: false };
    Some(match (size, opc) {
        (_, 0) => base,
        (_, 1) => MemOp { load: true, dst64: size == 3, ..base },
        (0 | 1, 2) => MemOp { load: true, signed: true, dst64: true, ..base },
        (0 | 1, 3) => MemOp { load: true, signed: true, dst64: false, ..base },
        (2, 2) => MemOp { load: true, signed: true, dst64: true, ..base },
        (3, 2) => MemOp { load: true, prefetch: true, ..base },
        _ => return None,
    })
}

fn fp_memop(size: u32, opc: u32) -> Option<MemOp> {
    let bytes = if opc & 2 != 0 {
        if size != 0 {
            return None;
        }
        16
    } else {
        1u8 << size
    };
    Some(MemOp { size: bytes, load: opc & 1 != 0, signed: false, dst64: false, fp: true, prefetch: false })
}

fn decode_ldst(w: u32, pc: u64) -> Insn {
    let rt = reg(w, 0);
    let rn = reg_sp(w, 5);
    let v = bit(w, 26);

    // Advanced SIMD structure load/store (multiple & single)
    if !bit(w, 31) && bits(w, 29, 25) == 0b00110 && v {
        return Insn::SimdLdSt(w);
    }

    // Exclusive / ordered / CAS
    if bits(w, 29, 24) == 0b001000 && !v {
        let size = 1u8 << bits(w, 31, 30);
        let o2 = bit(w, 23);
        let l = bit(w, 22);
        let o1 = bit(w, 21);
        let rs = reg(w, 16);
        let rt2 = reg(w, 10);
        return match (o2, o1) {
            (false, false) => {
                if l {
                    Insn::Ldx { size, pair: false, rt, rt2: ZR, rn }
                } else {
                    Insn::Stx { size, pair: false, rs, rt, rt2: ZR, rn }
                }
            }
            (false, true) => {
                if !bit(w, 31) {
                    // CASP: size<0> selects W/X pairs
                    let psize = if bit(w, 30) { 8 } else { 4 };
                    Insn::Casp { size: psize, rs, rt, rn }
                } else {
                    let psize = if bit(w, 30) { 8 } else { 4 };
                    if l {
                        Insn::Ldx { size: psize, pair: true, rt, rt2, rn }
                    } else {
                        Insn::Stx { size: psize, pair: true, rs, rt, rt2, rn }
                    }
                }
            }
            (true, false) => {
                if l {
                    Insn::Ldar { size, rt, rn }
                } else {
                    Insn::Stlr { size, rt, rn }
                }
            }
            (true, true) => Insn::Cas { size, rs, rt, rn },
        };
    }

    // Load register (literal)
    if bits(w, 29, 27) == 0b011 && !bit(w, 24) {
        let opc = bits(w, 31, 30);
        let imm = sext(bits(w, 23, 5) as u64, 19) << 2;
        let addr = pc.wrapping_add(imm as u64);
        let op = if v {
            match opc {
                0 => MemOp { size: 4, load: true, signed: false, dst64: false, fp: true, prefetch: false },
                1 => MemOp { size: 8, load: true, signed: false, dst64: false, fp: true, prefetch: false },
                2 => MemOp { size: 16, load: true, signed: false, dst64: false, fp: true, prefetch: false },
                _ => return Insn::Unknown(w),
            }
        } else {
            match opc {
                0 => MemOp { size: 4, load: true, signed: false, dst64: false, fp: false, prefetch: false },
                1 => MemOp { size: 8, load: true, signed: false, dst64: true, fp: false, prefetch: false },
                2 => MemOp { size: 4, load: true, signed: true, dst64: true, fp: false, prefetch: false },
                _ => MemOp { size: 8, load: true, signed: false, dst64: true, fp: false, prefetch: true },
            }
        };
        return Insn::LdLit { op, rt, addr };
    }

    // Load/store pair
    if bits(w, 29, 27) == 0b101 {
        let opc = bits(w, 31, 30);
        let l = bit(w, 22);
        let op = if v {
            let size = match opc {
                0 => 4,
                1 => 8,
                2 => 16,
                _ => return Insn::Unknown(w),
            };
            MemOp { size, load: l, signed: false, dst64: false, fp: true, prefetch: false }
        } else {
            match (opc, l) {
                (0, _) => MemOp { size: 4, load: l, signed: false, dst64: false, fp: false, prefetch: false },
                (1, true) => MemOp { size: 4, load: true, signed: true, dst64: true, fp: false, prefetch: false },
                (2, _) => MemOp { size: 8, load: l, signed: false, dst64: true, fp: false, prefetch: false },
                _ => return Insn::Unknown(w), // STGP etc.
            }
        };
        let imm = sext(bits(w, 21, 15) as u64, 7) * op.size as i64;
        let mode = match bits(w, 24, 23) {
            0b00 | 0b10 => AddrMode::Offset(imm),
            0b01 => AddrMode::Post(imm),
            _ => AddrMode::Pre(imm),
        };
        return Insn::LdStPair { op, rt, rt2: reg(w, 10), rn, mode };
    }

    // Load/store register
    if bits(w, 29, 27) == 0b111 {
        let size = bits(w, 31, 30);
        let opc = bits(w, 23, 22);
        let op = if v { fp_memop(size, opc) } else { gp_memop(size, opc) };

        if bit(w, 24) {
            // unsigned immediate offset
            let Some(op) = op else { return Insn::Unknown(w) };
            let imm = (bits(w, 21, 10) as i64) * op.size as i64;
            return Insn::LdSt { op, rt, rn, mode: AddrMode::Offset(imm) };
        }
        if !bit(w, 21) {
            let Some(op) = op else { return Insn::Unknown(w) };
            let imm = sext(bits(w, 20, 12) as u64, 9);
            let mode = match bits(w, 11, 10) {
                0b00 | 0b10 => AddrMode::Offset(imm), // LDUR / LDTR
                0b01 => AddrMode::Post(imm),
                _ => AddrMode::Pre(imm),
            };
            return Insn::LdSt { op, rt, rn, mode };
        }
        match bits(w, 11, 10) {
            0b10 => {
                let Some(op) = op else { return Insn::Unknown(w) };
                let option = bits(w, 15, 13);
                if option & 2 == 0 {
                    return Insn::Unknown(w);
                }
                let shift = if bit(w, 12) { op.size.trailing_zeros() as u8 } else { 0 };
                return Insn::LdSt { op, rt, rn, mode: AddrMode::Reg { rm: reg(w, 16), ext: extend_of(option), shift } };
            }
            0b00 if !v => {
                // atomic memory operations
                let bytes = 1u8 << size;
                let o3 = bit(w, 15);
                let aopc = bits(w, 14, 12);
                let rs = reg(w, 16);
                let aop = match (o3, aopc) {
                    (false, 0) => AtomicOp::Add,
                    (false, 1) => AtomicOp::Clr,
                    (false, 2) => AtomicOp::Eor,
                    (false, 3) => AtomicOp::Set,
                    (false, 4) => AtomicOp::Smax,
                    (false, 5) => AtomicOp::Smin,
                    (false, 6) => AtomicOp::Umax,
                    (false, 7) => AtomicOp::Umin,
                    (true, 0) => AtomicOp::Swp,
                    (true, 4) if rs == 31 => return Insn::Ldar { size: bytes, rt, rn }, // LDAPR
                    _ => return Insn::Unknown(w),
                };
                return Insn::Atomic { size: bytes, op: aop, rs, rt, rn };
            }
            _ => return Insn::Unknown(w),
        }
    }

    // LDAPR/STLR unscaled (RCpc immediate): bits 29:24 = 011001
    if bits(w, 29, 24) == 0b011001 && !v && bits(w, 11, 10) == 0 && !bit(w, 21) {
        let size = 1u8 << bits(w, 31, 30);
        let opc = bits(w, 23, 22);
        let imm = sext(bits(w, 20, 12) as u64, 9);
        let op = match opc {
            0 => MemOp { size, load: false, signed: false, dst64: false, fp: false, prefetch: false },
            1 => MemOp { size, load: true, signed: false, dst64: size == 8, fp: false, prefetch: false },
            2 => MemOp { size, load: true, signed: true, dst64: true, fp: false, prefetch: false },
            _ => MemOp { size, load: true, signed: true, dst64: false, fp: false, prefetch: false },
        };
        return Insn::LdSt { op, rt, rn, mode: AddrMode::Offset(imm) };
    }

    Insn::Unknown(w)
}

fn decode_dp_reg(w: u32) -> Insn {
    let sf = bit(w, 31);
    let rd = reg(w, 0);
    let rn = reg(w, 5);
    let rm = reg(w, 16);
    let op1 = bit(w, 28);

    if !op1 {
        if !bit(w, 24) {
            // logical (shifted register)
            let amount = bits(w, 15, 10) as u8;
            if !sf && amount >= 32 {
                return Insn::Unknown(w);
            }
            let op = match bits(w, 30, 29) {
                0 => LogicOp::And,
                1 => LogicOp::Orr,
                2 => LogicOp::Eor,
                _ => LogicOp::Ands,
            };
            return Insn::LogicReg { sf, op, invert: bit(w, 21), shift: shift_of(bits(w, 23, 22)), amount, rd, rn, rm };
        }
        let sub = bit(w, 30);
        let s = bit(w, 29);
        if !bit(w, 21) {
            let shift = bits(w, 23, 22);
            let amount = bits(w, 15, 10) as u8;
            if shift == 3 || (!sf && amount >= 32) {
                return Insn::Unknown(w);
            }
            return Insn::AddSubShift { sf, sub, s, shift: shift_of(shift), amount, rd, rn, rm };
        }
        if bits(w, 23, 22) != 0 {
            return Insn::Unknown(w);
        }
        let amount = bits(w, 12, 10) as u8;
        if amount > 4 {
            return Insn::Unknown(w);
        }
        return Insn::AddSubExt {
            sf,
            sub,
            s,
            ext: extend_of(bits(w, 15, 13)),
            amount,
            rd: if s { rd } else { reg_sp(w, 0) },
            rn: reg_sp(w, 5),
            rm,
        };
    }

    if bit(w, 24) {
        // data processing (3 source)
        let op54 = bits(w, 30, 29);
        let op31 = bits(w, 23, 21);
        let o0 = bit(w, 15);
        let ra = reg(w, 10);
        if op54 != 0 {
            return Insn::Unknown(w);
        }
        let op = match (op31, o0, sf) {
            (0, false, _) => Dp3Op::Madd,
            (0, true, _) => Dp3Op::Msub,
            (1, false, true) => Dp3Op::Smaddl,
            (1, true, true) => Dp3Op::Smsubl,
            (2, false, true) => Dp3Op::Smulh,
            (5, false, true) => Dp3Op::Umaddl,
            (5, true, true) => Dp3Op::Umsubl,
            (6, false, true) => Dp3Op::Umulh,
            _ => return Insn::Unknown(w),
        };
        return Insn::Dp3 { sf, op, rd, rn, rm, ra };
    }

    match bits(w, 23, 21) {
        0b000 => {
            // add/sub with carry
            if bits(w, 15, 10) != 0 {
                return Insn::Unknown(w);
            }
            Insn::Adc { sf, sub: bit(w, 30), s: bit(w, 29), rd, rn, rm }
        }
        0b010 => {
            // conditional compare
            if !bit(w, 29) || bit(w, 10) || bit(w, 4) {
                return Insn::Unknown(w);
            }
            let imm = if bit(w, 11) { Some(rm) } else { None };
            Insn::CondCmp { sf, sub: bit(w, 30), rn, rm, imm, cond: bits(w, 15, 12) as u8, nzcv: bits(w, 3, 0) as u8 }
        }
        0b100 => {
            if bit(w, 29) {
                return Insn::Unknown(w);
            }
            let op = match (bit(w, 30), bits(w, 11, 10)) {
                (false, 0) => CselOp::Csel,
                (false, 1) => CselOp::Csinc,
                (true, 0) => CselOp::Csinv,
                (true, 1) => CselOp::Csneg,
                _ => return Insn::Unknown(w),
            };
            Insn::CondSel { sf, op, rd, rn, rm, cond: bits(w, 15, 12) as u8 }
        }
        0b110 => {
            let opcode = bits(w, 15, 10);
            if bit(w, 30) {
                // 1-source
                if bits(w, 20, 16) != 0 || bit(w, 29) {
                    return Insn::Unknown(w);
                }
                let op = match (opcode, sf) {
                    (0, _) => Dp1Op::Rbit,
                    (1, _) => Dp1Op::Rev16,
                    (2, false) => Dp1Op::Rev,
                    (2, true) => Dp1Op::Rev32,
                    (3, true) => Dp1Op::Rev,
                    (4, _) => Dp1Op::Clz,
                    (5, _) => Dp1Op::Cls,
                    (6, _) => Dp1Op::Ctz,
                    (7, _) => Dp1Op::Cnt,
                    (8, _) => Dp1Op::Abs,
                    _ => return Insn::Unknown(w),
                };
                Insn::Dp1 { sf, op, rd, rn }
            } else {
                if bit(w, 29) {
                    return Insn::Unknown(w);
                }
                let op = match opcode {
                    0b000010 => Dp2Op::Udiv,
                    0b000011 => Dp2Op::Sdiv,
                    0b001000 => Dp2Op::Lslv,
                    0b001001 => Dp2Op::Lsrv,
                    0b001010 => Dp2Op::Asrv,
                    0b001011 => Dp2Op::Rorv,
                    0b010000..=0b010111 => Dp2Op::Crc32 { size: 1 << (opcode & 3), c: opcode & 4 != 0 },
                    0b011000 => Dp2Op::Smax,
                    0b011001 => Dp2Op::Umax,
                    0b011010 => Dp2Op::Smin,
                    0b011011 => Dp2Op::Umin,
                    _ => return Insn::Unknown(w),
                };
                Insn::Dp2 { sf, op, rd, rn, rm }
            }
        }
        _ => Insn::Unknown(w),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmasks() {
        // and x0, x0, #0xff  -> N=1 immr=0 imms=7
        assert_eq!(decode_bit_masks(1, 7, 0, true), Some(0xff));
        // orr w0, wzr, #0x55555555 -> N=0, immr=0, imms=0b111100
        assert_eq!(decode_bit_masks(0, 0b111100, 0, false), Some(0x5555_5555));
        // and x0, x0, #0xfffffffffffffff0 -> N=1 immr=60 imms=59
        assert_eq!(decode_bit_masks(1, 59, 60, true), Some(0xffff_ffff_ffff_fff0));
    }

    #[test]
    fn basic_decodes() {
        // add x0, x1, #16
        assert_eq!(
            decode(0x91004020, 0),
            Insn::AddSubImm { sf: true, sub: false, s: false, rd: 0, rn: 1, imm: 16 }
        );
        // mov x29, sp  == add x29, sp, #0
        assert_eq!(
            decode(0x910003fd, 0),
            Insn::AddSubImm { sf: true, sub: false, s: false, rd: 29, rn: SP, imm: 0 }
        );
        // ret
        assert_eq!(decode(0xd65f03c0, 0), Insn::BrReg { rn: 30, link: false });
        // svc #0
        assert_eq!(decode(0xd4000001, 0), Insn::Svc { imm: 0 });
        // bl +8 at 0x1000
        assert_eq!(decode(0x94000002, 0x1000), Insn::B { target: 0x1008, link: true });
        // stp x29, x30, [sp, #-16]!
        assert_eq!(
            decode(0xa9bf7bfd, 0),
            Insn::LdStPair {
                op: MemOp { size: 8, load: false, signed: false, dst64: true, fp: false, prefetch: false },
                rt: 29,
                rt2: 30,
                rn: SP,
                mode: AddrMode::Pre(-16)
            }
        );
        // ldr x0, [x1, #8]
        assert_eq!(
            decode(0xf9400420, 0),
            Insn::LdSt {
                op: MemOp { size: 8, load: true, signed: false, dst64: true, fp: false, prefetch: false },
                rt: 0,
                rn: 1,
                mode: AddrMode::Offset(8)
            }
        );
        // adrp x0, 0x2000 at pc 0x1234
        assert_eq!(decode(0x90000010 | 0, 0x1234), Insn::Adr { rd: 16, value: 0x1000 + 0 });
        // cmp x0, #1 == subs xzr, x0, #1
        assert_eq!(
            decode(0xf100041f, 0),
            Insn::AddSubImm { sf: true, sub: true, s: true, rd: ZR, rn: 0, imm: 1 }
        );
        // mrs x0, tpidr_el0
        assert_eq!(decode(0xd53bd040, 0), Insn::Mrs { rt: 0, reg: sysregs::TPIDR_EL0 });
        // ldaxr w1, [x0]
        assert_eq!(decode(0x885ffc01, 0), Insn::Ldx { size: 4, pair: false, rt: 1, rt2: ZR, rn: 0 });
        // stlxr w2, w1, [x0]
        assert_eq!(decode(0x8802fc01, 0), Insn::Stx { size: 4, pair: false, rs: 2, rt: 1, rt2: ZR, rn: 0 });
    }
}
