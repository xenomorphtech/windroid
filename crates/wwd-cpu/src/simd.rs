//! Advanced SIMD and floating-point interpreter.
//!
//! Decoded straight from the instruction word (these are not routed through
//! `decode::Insn` because the encoding space is huge and mostly regular).
//! Unimplemented encodings return `false`, which raises an Undefined exit
//! that reports the word so coverage can be grown on demand.

use crate::cpu::*;
use crate::interp::{load, load128, store, store128};

#[inline(always)]
fn bits(w: u32, hi: u32, lo: u32) -> u32 {
    (w >> lo) & ((1u32 << (hi - lo + 1)) - 1)
}
#[inline(always)]
fn bit(w: u32, b: u32) -> bool {
    (w >> b) & 1 != 0
}
#[inline(always)]
fn emask(es: u32) -> u64 {
    if es >= 64 {
        u64::MAX
    } else {
        (1u64 << es) - 1
    }
}
#[inline(always)]
fn lane(v: u128, es: u32, i: u32) -> u64 {
    ((v >> (i * es)) as u64) & emask(es)
}
#[inline(always)]
fn set_lane(v: &mut u128, es: u32, i: u32, x: u64) {
    let m = (emask(es) as u128) << (i * es);
    *v = (*v & !m) | (((x & emask(es)) as u128) << (i * es));
}
#[inline(always)]
fn sext(x: u64, es: u32) -> i64 {
    if es >= 64 {
        x as i64
    } else {
        ((x << (64 - es)) as i64) >> (64 - es)
    }
}
#[inline(always)]
fn trunc_q(v: u128, q: bool) -> u128 {
    if q {
        v
    } else {
        v & (u64::MAX as u128)
    }
}

// ---------------------------------------------------------------------------
// scalar FP helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Ft {
    S,
    D,
    H,
}

fn ftype(t: u32) -> Option<Ft> {
    match t {
        0 => Some(Ft::S),
        1 => Some(Ft::D),
        3 => Some(Ft::H),
        _ => None,
    }
}

fn rd_f(cpu: &Cpu, r: u32, t: Ft) -> f64 {
    let v = cpu.v[r as usize];
    match t {
        Ft::S => f32::from_bits(v as u32) as f64,
        Ft::D => f64::from_bits(v as u64),
        Ft::H => f16_to_f64(v as u16),
    }
}

fn wr_f(cpu: &mut Cpu, r: u32, t: Ft, x: f64) {
    cpu.v[r as usize] = match t {
        Ft::S => (x as f32).to_bits() as u128,
        Ft::D => x.to_bits() as u128,
        Ft::H => f64_to_f16(x) as u128,
    };
}

fn f16_to_f64(h: u16) -> f64 {
    let s = (h >> 15) & 1;
    let e = ((h >> 10) & 0x1f) as i32;
    let m = (h & 0x3ff) as f64;
    let v = if e == 0 {
        m * 2f64.powi(-24)
    } else if e == 31 {
        if m == 0.0 {
            f64::INFINITY
        } else {
            f64::NAN
        }
    } else {
        (1.0 + m / 1024.0) * 2f64.powi(e - 15)
    };
    if s == 1 {
        -v
    } else {
        v
    }
}

fn f64_to_f16(x: f64) -> u16 {
    let f = x as f32;
    let b = f.to_bits();
    let s = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xff) as i32;
    let m = b & 0x7f_ffff;
    if e == 255 {
        return s | 0x7c00 | if m != 0 { 0x200 } else { 0 };
    }
    let ne = e - 127 + 15;
    if ne >= 31 {
        return s | 0x7c00;
    }
    if ne <= 0 {
        if ne < -10 {
            return s;
        }
        let m = m | 0x80_0000;
        let shift = (14 - ne) as u32;
        let mut hm = (m >> shift) as u16;
        let rem = m & ((1 << shift) - 1);
        let half = 1 << (shift - 1);
        if rem > half || (rem == half && hm & 1 == 1) {
            hm += 1;
        }
        return s | hm;
    }
    let mut hm = (m >> 13) as u16;
    let rem = m & 0x1fff;
    let mut he = ne as u16;
    if rem > 0x1000 || (rem == 0x1000 && hm & 1 == 1) {
        hm += 1;
        if hm == 0x400 {
            hm = 0;
            he += 1;
            if he >= 31 {
                return s | 0x7c00;
            }
        }
    }
    s | (he << 10) | hm
}

fn fcmp_flags(a: f64, b: f64) -> u64 {
    if a.is_nan() || b.is_nan() {
        0b0011
    } else if a == b {
        0b0110
    } else if a < b {
        0b1000
    } else {
        0b0010
    }
}

fn fmax(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_negative() && b.is_sign_negative() {
            a
        } else {
            a.abs()
        }
    } else {
        a.max(b)
    }
}

fn fmin(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_negative() || b.is_sign_negative() {
            -(a.abs())
        } else {
            a
        }
    } else {
        a.min(b)
    }
}

/// Round according to rmode: 0 = nearest-even, 1 = +inf, 2 = -inf, 3 = zero, 4 = ties-away.
fn round_mode(x: f64, mode: u32) -> f64 {
    match mode {
        0 => x.round_ties_even(),
        1 => x.ceil(),
        2 => x.floor(),
        3 => x.trunc(),
        _ => x.round(),
    }
}

fn fp_to_int(x: f64, mode: u32, signed: bool, sf: bool) -> u64 {
    let r = round_mode(x, mode);
    match (signed, sf) {
        (true, true) => r as i64 as u64,
        (true, false) => r as i32 as u32 as u64,
        (false, true) => r as u64,
        (false, false) => r as u32 as u64,
    }
}

/// Single-precision arithmetic must round to single after every op; doing the
/// math in f64 and rounding once is exact for + - * / sqrt (double has
/// > 2*24+2 bits), so `as f32` after the f64 op is correctly rounded.
fn fp_bin(op: u32, a: f64, b: f64) -> Option<f64> {
    Some(match op {
        0 => a * b,
        1 => a / b,
        2 => a + b,
        3 => a - b,
        4 => fmax(a, b),
        5 => fmin(a, b),
        6 => {
            if a.is_nan() {
                b
            } else if b.is_nan() {
                a
            } else {
                fmax(a, b)
            }
        }
        7 => {
            if a.is_nan() {
                b
            } else if b.is_nan() {
                a
            } else {
                fmin(a, b)
            }
        }
        8 => -(a * b),
        _ => return None,
    })
}

fn vfp_expand_imm(imm8: u32, t: Ft) -> u64 {
    let sign = ((imm8 >> 7) & 1) as u64;
    let b6 = ((imm8 >> 6) & 1) as u64;
    let e = ((imm8 >> 4) & 3) as u64;
    let f = (imm8 & 15) as u64;
    match t {
        Ft::D => {
            let exp = ((b6 ^ 1) << 10) | (if b6 == 1 { 0xff << 2 } else { 0 }) | e;
            (sign << 63) | (exp << 52) | (f << 48)
        }
        Ft::S => {
            let exp = ((b6 ^ 1) << 7) | (if b6 == 1 { 0x1f << 2 } else { 0 }) | e;
            (sign << 31) | (exp << 23) | (f << 19)
        }
        Ft::H => {
            let exp = ((b6 ^ 1) << 4) | (if b6 == 1 { 0x3 << 2 } else { 0 }) | e;
            (sign << 15) | (exp << 10) | (f << 6)
        }
    }
}

fn adv_simd_expand_imm(op: bool, cmode: u32, imm8: u64) -> u64 {
    let rep32 = |x: u64| x | (x << 32);
    let rep16 = |x: u64| x | (x << 16) | (x << 32) | (x << 48);
    match cmode >> 1 {
        0 => rep32(imm8),
        1 => rep32(imm8 << 8),
        2 => rep32(imm8 << 16),
        3 => rep32(imm8 << 24),
        4 => rep16(imm8),
        5 => rep16(imm8 << 8),
        6 => {
            if cmode & 1 == 0 {
                rep32((imm8 << 8) | 0xff)
            } else {
                rep32((imm8 << 16) | 0xffff)
            }
        }
        _ => {
            if cmode & 1 == 0 {
                if !op {
                    imm8 * 0x0101_0101_0101_0101
                } else {
                    let mut r = 0u64;
                    for i in 0..8 {
                        if imm8 >> i & 1 != 0 {
                            r |= 0xff << (i * 8);
                        }
                    }
                    r
                }
            } else if !op {
                let s = vfp_expand_imm(imm8 as u32, Ft::S);
                rep32(s)
            } else {
                vfp_expand_imm(imm8 as u32, Ft::D)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// entry points
// ---------------------------------------------------------------------------

pub fn exec(cpu: &mut Cpu, w: u32) -> bool {
    if bit(w, 28) {
        // scalar FP & conversions; also AdvSIMD scalar (bits 31:30 = 01)
        if bits(w, 31, 30) == 0b01 {
            return simd_scalar(cpu, w);
        }
        return fp_scalar(cpu, w);
    }
    simd_vector(cpu, w)
}

fn fp_scalar(cpu: &mut Cpu, w: u32) -> bool {
    let rd = bits(w, 4, 0);
    let rn = bits(w, 9, 5);
    let rm = bits(w, 20, 16);
    if bit(w, 30) {
        return false;
    }
    // FP data-processing (3 source)
    if bits(w, 28, 24) == 0b11111 {
        let Some(t) = ftype(bits(w, 23, 22)) else { return false };
        let ra = bits(w, 14, 10);
        let (a, n, m) = (rd_f(cpu, ra, t), rd_f(cpu, rn, t), rd_f(cpu, rm, t));
        let r = match (bit(w, 21), bit(w, 15)) {
            (false, false) => n.mul_add(m, a),
            (false, true) => (-n).mul_add(m, a),
            (true, false) => (-n).mul_add(m, -a),
            (true, true) => n.mul_add(m, -a),
        };
        // f64 fma then round to f32 is double rounding for S; acceptable for now
        let r = if t == Ft::S { (n as f32).mul_add_sel(m as f32, a as f32, bit(w, 21), bit(w, 15)) as f64 } else { r };
        wr_f(cpu, rd, t, r);
        return true;
    }
    if bits(w, 28, 24) != 0b11110 {
        return false;
    }
    let sf = bit(w, 31);
    let tt = bits(w, 23, 22);

    if !bit(w, 21) {
        // conversion between FP and fixed-point
        let Some(t) = ftype(tt) else { return false };
        let rmode = bits(w, 20, 19);
        let opcode = bits(w, 18, 16);
        let fbits = 64 - bits(w, 15, 10);
        let scale = 2f64.powi(fbits as i32);
        match (rmode, opcode) {
            (0, 2) | (0, 3) => {
                let v = cpu.rs(rn as u8, sf);
                let x = if opcode == 2 {
                    if sf {
                        v as i64 as f64
                    } else {
                        v as u32 as i32 as f64
                    }
                } else {
                    v as f64
                };
                wr_f(cpu, rd, t, x / scale);
            }
            (3, 0) | (3, 1) => {
                let x = rd_f(cpu, rn, t) * scale;
                let r = fp_to_int(x, 3, opcode == 0, sf);
                cpu.set(rd as u8, r);
            }
            _ => return false,
        }
        return true;
    }

    // FP <-> integer conversions
    if bits(w, 15, 10) == 0 {
        let rmode = bits(w, 20, 19);
        let opcode = bits(w, 18, 16);
        match (opcode, rmode) {
            (6, 0) | (7, 0) | (6, 1) | (7, 1) => {
                // FMOV between general and FP registers
                let to_fp = opcode == 7;
                let top = rmode == 1; // FMOV Xd, Vn.D[1] / FMOV Vd.D[1], Xn
                if to_fp {
                    let x = cpu.r(rn as u8);
                    let r = &mut cpu.v[rd as usize];
                    if top {
                        *r = (*r & (u64::MAX as u128)) | ((x as u128) << 64);
                    } else {
                        *r = match tt {
                            0 => (x & 0xffff_ffff) as u128,
                            3 => (x & 0xffff) as u128,
                            _ => x as u128,
                        };
                    }
                } else {
                    let v = cpu.v[rn as usize];
                    let x = if top {
                        (v >> 64) as u64
                    } else {
                        match tt {
                            0 => v as u32 as u64,
                            3 => v as u16 as u64,
                            _ => v as u64,
                        }
                    };
                    cpu.set(rd as u8, x);
                }
                return true;
            }
            _ => {}
        }
        let Some(t) = ftype(tt) else { return false };
        let x = rd_f(cpu, rn, t);
        let r = match (rmode, opcode) {
            (0, 0) => fp_to_int(x, 0, true, sf),
            (0, 1) => fp_to_int(x, 0, false, sf),
            (0, 4) => fp_to_int(x, 4, true, sf),
            (0, 5) => fp_to_int(x, 4, false, sf),
            (1, 0) => fp_to_int(x, 1, true, sf),
            (1, 1) => fp_to_int(x, 1, false, sf),
            (2, 0) => fp_to_int(x, 2, true, sf),
            (2, 1) => fp_to_int(x, 2, false, sf),
            (3, 0) => fp_to_int(x, 3, true, sf),
            (3, 1) => fp_to_int(x, 3, false, sf),
            (0, 2) | (0, 3) => {
                let v = cpu.rs(rn as u8, sf);
                let f = if opcode == 2 {
                    if sf {
                        v as i64 as f64
                    } else {
                        v as u32 as i32 as f64
                    }
                } else {
                    v as f64
                };
                // i64 -> f32 must round once from the integer
                if t == Ft::S {
                    let f32v = if opcode == 2 {
                        if sf {
                            v as i64 as f32
                        } else {
                            v as u32 as i32 as f32
                        }
                    } else if sf {
                        v as f32
                    } else {
                        v as u32 as f32
                    };
                    cpu.v[rd as usize] = f32v.to_bits() as u128;
                } else {
                    wr_f(cpu, rd, t, f);
                }
                return true;
            }
            _ => return false,
        };
        cpu.set(rd as u8, r);
        return true;
    }

    let Some(t) = ftype(tt) else { return false };

    // FP data-processing (1 source)
    if bits(w, 14, 10) == 0b10000 {
        let opcode = bits(w, 20, 15);
        let x = rd_f(cpu, rn, t);
        match opcode {
            0 => cpu.v[rd as usize] = cpu.v[rn as usize] & fmask(t),
            1 => {
                let v = cpu.v[rn as usize] & fmask(t) & !sign_bit(t);
                cpu.v[rd as usize] = v;
            }
            2 => {
                let v = (cpu.v[rn as usize] ^ sign_bit(t)) & fmask(t);
                cpu.v[rd as usize] = v;
            }
            3 => {
                let r = if t == Ft::S { (x as f32).sqrt() as f64 } else { x.sqrt() };
                wr_f(cpu, rd, t, r);
            }
            4 => wr_f(cpu, rd, Ft::S, x),
            5 => wr_f(cpu, rd, Ft::D, x),
            7 => wr_f(cpu, rd, Ft::H, x),
            8..=15 => {
                let mode = match opcode - 8 {
                    0 => 0,
                    1 => 1,
                    2 => 2,
                    3 => 3,
                    4 => 4,
                    _ => 0, // X, I: current rounding mode (assume RNE)
                };
                wr_f(cpu, rd, t, round_mode(x, mode));
            }
            _ => return false,
        }
        return true;
    }
    // FP compare
    if bits(w, 13, 10) == 0b1000 {
        let a = rd_f(cpu, rn, t);
        let b = if bit(w, 3) { 0.0 } else { rd_f(cpu, rm, t) };
        cpu.nzcv = fcmp_flags(a, b);
        return true;
    }
    // FP immediate
    if bits(w, 12, 10) == 0b100 {
        let imm8 = bits(w, 20, 13);
        cpu.v[rd as usize] = vfp_expand_imm(imm8, t) as u128;
        return true;
    }
    match bits(w, 11, 10) {
        0b01 => {
            // FCCMP / FCCMPE
            let cond = bits(w, 15, 12) as u8;
            if cond_holds(cond, cpu.nzcv) {
                cpu.nzcv = fcmp_flags(rd_f(cpu, rn, t), rd_f(cpu, rm, t));
            } else {
                cpu.nzcv = bits(w, 3, 0) as u64;
            }
            true
        }
        0b10 => {
            let op = bits(w, 15, 12);
            let (a, b) = (rd_f(cpu, rn, t), rd_f(cpu, rm, t));
            let r = if t == Ft::S {
                let (a, b) = (a as f32, b as f32);
                match op {
                    0 => (a * b) as f64,
                    1 => (a / b) as f64,
                    2 => (a + b) as f64,
                    3 => (a - b) as f64,
                    8 => (-(a * b)) as f64,
                    _ => match fp_bin(op, a as f64, b as f64) {
                        Some(r) => r,
                        None => return false,
                    },
                }
            } else {
                match fp_bin(op, a, b) {
                    Some(r) => r,
                    None => return false,
                }
            };
            wr_f(cpu, rd, t, r);
            true
        }
        _ => {
            // FCSEL
            let cond = bits(w, 15, 12) as u8;
            let src = if cond_holds(cond, cpu.nzcv) { rn } else { rm };
            cpu.v[rd as usize] = cpu.v[src as usize] & fmask(t);
            true
        }
    }
}

trait MulAddSel {
    fn mul_add_sel(self, m: f32, a: f32, o1: bool, o0: bool) -> f32;
}
impl MulAddSel for f32 {
    fn mul_add_sel(self, m: f32, a: f32, o1: bool, o0: bool) -> f32 {
        match (o1, o0) {
            (false, false) => self.mul_add(m, a),
            (false, true) => (-self).mul_add(m, a),
            (true, false) => (-self).mul_add(m, -a),
            (true, true) => self.mul_add(m, -a),
        }
    }
}

fn fmask(t: Ft) -> u128 {
    match t {
        Ft::S => 0xffff_ffff,
        Ft::D => u64::MAX as u128,
        Ft::H => 0xffff,
    }
}
fn sign_bit(t: Ft) -> u128 {
    match t {
        Ft::S => 1 << 31,
        Ft::D => 1 << 63,
        Ft::H => 1 << 15,
    }
}

/// AdvSIMD scalar forms (bits 31:30 = 01, bit 28 = 1).
fn simd_scalar(cpu: &mut Cpu, w: u32) -> bool {
    let rd = bits(w, 4, 0) as usize;
    let rn = bits(w, 9, 5) as usize;
    let rm = bits(w, 20, 16) as usize;
    let u = bit(w, 29);
    let size = bits(w, 23, 22);
    // scalar copy: DUP (element) -> MOV b/h/s/d
    if bits(w, 28, 21) == 0b1111_0000 && bits(w, 15, 10) == 0b000001 && !u {
        let imm5 = bits(w, 20, 16);
        let sz = imm5.trailing_zeros();
        if sz > 3 {
            return false;
        }
        let es = 8 << sz;
        let idx = imm5 >> (sz + 1);
        cpu.v[rd] = lane(cpu.v[rn], es, idx) as u128;
        return true;
    }
    // scalar x indexed element: FMUL/FMLA/FMLS d0, d1, v2.d[i]
    if bits(w, 28, 24) == 0b11111 && !bit(w, 10) {
        return by_element(cpu, w, true);
    }
    // scalar pairwise: ADDP d, v.2d ; FADDP etc.
    if bits(w, 28, 24) == 0b11110 && bits(w, 21, 17) == 0b11000 && bits(w, 11, 10) == 0b10 {
        let opcode = bits(w, 16, 12);
        let v = cpu.v[rn];
        if !u && opcode == 0b11011 && size == 3 {
            cpu.v[rd] = (lane(v, 64, 0).wrapping_add(lane(v, 64, 1))) as u128;
            return true;
        }
        if u && opcode == 0b01101 {
            // FADDP scalar
            if size & 1 == 0 {
                let a = f32::from_bits(lane(v, 32, 0) as u32);
                let b = f32::from_bits(lane(v, 32, 1) as u32);
                cpu.v[rd] = (a + b).to_bits() as u128;
            } else {
                let a = f64::from_bits(lane(v, 64, 0));
                let b = f64::from_bits(lane(v, 64, 1));
                cpu.v[rd] = (a + b).to_bits() as u128;
            }
            return true;
        }
        return false;
    }
    // scalar three same (64-bit integer ops on d registers)
    if bits(w, 28, 24) == 0b11110 && bit(w, 21) && bit(w, 10) {
        let opcode = bits(w, 15, 11);
        let a = cpu.v[rn] as u64;
        let b = cpu.v[rm] as u64;
        if size == 3 && opcode < 0b11000 {
            let r = match (u, opcode) {
                (false, 0b10000) => a.wrapping_add(b),
                (true, 0b10000) => a.wrapping_sub(b),
                (true, 0b10001) => if a == b { u64::MAX } else { 0 },
                (false, 0b10001) => if a & b != 0 { u64::MAX } else { 0 },
                (false, 0b00110) => if (a as i64) > (b as i64) { u64::MAX } else { 0 },
                (true, 0b00110) => if a > b { u64::MAX } else { 0 },
                (false, 0b00111) => if (a as i64) >= (b as i64) { u64::MAX } else { 0 },
                (true, 0b00111) => if a >= b { u64::MAX } else { 0 },
                (true, 0b01000) => ushl(a, b as i8 as i64, 64),
                (false, 0b01000) => sshl(a, b as i8 as i64, 64),
                _ => return false,
            };
            cpu.v[rd] = r as u128;
            return true;
        }
        // scalar FP three-same (FABD, FCMEQ, ...)
        let fsz = if size & 1 == 0 { 32 } else { 64 };
        let fa = lane_f(cpu.v[rn], fsz, 0);
        let fb = lane_f(cpu.v[rm], fsz, 0);
        let r = match (u, bit(w, 23), opcode) {
            (true, true, 0b11010) => Some(set_f((fa - fb).abs(), fsz)),
            (false, false, 0b11111) => Some(set_f(recip_step(fa, fb, fsz), fsz)),
            (false, true, 0b11111) => Some(set_f(rsqrt_step(fa, fb, fsz), fsz)),
            (true, false, 0b11101) => Some(mask_cmp(fa.abs() >= fb.abs(), fsz)),
            (true, true, 0b11101) => Some(mask_cmp(fa.abs() > fb.abs(), fsz)),
            (false, false, 0b11100) => Some(mask_cmp(fa == fb, fsz)),
            (true, false, 0b11100) => Some(mask_cmp(fa >= fb, fsz)),
            (true, true, 0b11100) => Some(mask_cmp(fa > fb, fsz)),
            _ => None,
        };
        if let Some(r) = r {
            cpu.v[rd] = r as u128;
            return true;
        }
        return false;
    }
    // scalar shift by immediate
    if bits(w, 28, 23) == 0b111110 && bit(w, 10) {
        let immh = bits(w, 22, 19);
        let immb = bits(w, 18, 16);
        let opcode = bits(w, 15, 11);
        if immh & 8 == 0 {
            return false;
        }
        let a = cpu.v[rn] as u64;
        let sh_r = 128 - ((immh << 3) | immb);
        let sh_l = ((immh << 3) | immb) - 64;
        let r = match (u, opcode) {
            (false, 0b01010) => a << sh_l,
            (true, 0b00000) => if sh_r >= 64 { 0 } else { a >> sh_r },
            (false, 0b00000) => ((a as i64) >> sh_r.min(63)) as u64,
            _ => return false,
        };
        cpu.v[rd] = r as u128;
        return true;
    }
    // scalar two-reg misc: SCVTF/UCVTF (vector form, scalar), FCVTZS, CMEQ #0...
    if bits(w, 28, 24) == 0b11110 && bits(w, 21, 17) == 0b10000 && bits(w, 11, 10) == 0b10 {
        let opcode = bits(w, 16, 12);
        let fsz = if size & 1 == 0 { 32 } else { 64 };
        let v = cpu.v[rn];
        match (u, bit(w, 23), opcode) {
            (false, false, 0b11101) | (true, false, 0b11101) => {
                // SCVTF / UCVTF scalar
                let x = lane(v, fsz, 0);
                let f = if !u { sext(x, fsz) as f64 } else { x as f64 };
                let f = if fsz == 32 {
                    if !u { (sext(x, 32) as i32) as f32 } else { x as u32 as f32 }.to_bits() as u64
                } else {
                    f.to_bits()
                };
                cpu.v[rd] = f as u128;
                true
            }
            (false, true, 0b11101) | (true, true, 0b11101) => {
                // FRECPE / FRSQRTE scalar
                let x = lane(v, fsz, 0);
                let r = if !u { recip_estimate_bits(x, fsz) } else { rsqrt_estimate_bits(x, fsz) };
                cpu.v[rd] = r as u128;
                true
            }
            (false, true, 0b11011) | (true, true, 0b11011) => {
                // FCVTZS / FCVTZU scalar
                let f = lane_f(v, fsz, 0);
                let r = fp_to_int(f, 3, !u, fsz == 64);
                cpu.v[rd] = (r & emask(fsz)) as u128;
                true
            }
            (false, _, 0b01001) if size == 3 => {
                cpu.v[rd] = if v as u64 == 0 { u64::MAX as u128 } else { 0 };
                true
            }
            (true, _, 0b01011) if size == 3 => {
                cpu.v[rd] = (v as u64).wrapping_neg() as u128;
                true
            }
            _ => false,
        }
    } else {
        false
    }
}

fn lane_f(v: u128, es: u32, i: u32) -> f64 {
    if es == 32 {
        f32::from_bits(lane(v, 32, i) as u32) as f64
    } else {
        f64::from_bits(lane(v, 64, i))
    }
}
fn set_f(x: f64, es: u32) -> u64 {
    if es == 32 {
        (x as f32).to_bits() as u64
    } else {
        x.to_bits()
    }
}
fn mask_cmp(c: bool, es: u32) -> u64 {
    if c {
        emask(es)
    } else {
        0
    }
}

fn ushl(a: u64, sh: i64, es: u32) -> u64 {
    let a = a & emask(es);
    if sh >= es as i64 || sh <= -(es as i64) {
        0
    } else if sh >= 0 {
        (a << sh) & emask(es)
    } else {
        a >> (-sh)
    }
}
fn sshl(a: u64, sh: i64, es: u32) -> u64 {
    let s = sext(a, es);
    let r = if sh >= es as i64 {
        0
    } else if sh >= 0 {
        s << sh
    } else if -sh >= es as i64 {
        if s < 0 {
            -1
        } else {
            0
        }
    } else {
        s >> (-sh)
    };
    (r as u64) & emask(es)
}

fn simd_vector(cpu: &mut Cpu, w: u32) -> bool {
    if bit(w, 31) {
        return false;
    }
    let q = bit(w, 30);
    let u = bit(w, 29);
    let rd = bits(w, 4, 0) as usize;
    let rn = bits(w, 9, 5) as usize;
    let rm = bits(w, 20, 16) as usize;
    let size = bits(w, 23, 22);
    let ds: u32 = if q { 128 } else { 64 };

    // ---- copy: DUP/INS/UMOV/SMOV ----
    if bits(w, 28, 21) == 0b0111_0000 && bit(w, 10) && !bit(w, 15) {
        let imm5 = bits(w, 20, 16);
        let imm4 = bits(w, 14, 11);
        let sz = imm5.trailing_zeros();
        if sz > 3 {
            return false;
        }
        let es = 8u32 << sz;
        let idx = imm5 >> (sz + 1);
        if u {
            // INS (element)
            if !q {
                return false;
            }
            let src_idx = imm4 >> sz;
            let x = lane(cpu.v[rn], es, src_idx);
            set_lane(&mut cpu.v[rd], es, idx, x);
            return true;
        }
        match imm4 {
            0b0000 => {
                // DUP (element)
                let x = lane(cpu.v[rn], es, idx);
                let mut r = 0u128;
                for i in 0..ds / es {
                    set_lane(&mut r, es, i, x);
                }
                cpu.v[rd] = r;
            }
            0b0001 => {
                // DUP (general)
                let x = cpu.r(rn as u8);
                let mut r = 0u128;
                for i in 0..ds / es {
                    set_lane(&mut r, es, i, x);
                }
                cpu.v[rd] = r;
            }
            0b0011 => {
                // INS (general)
                let x = cpu.r(rn as u8);
                set_lane(&mut cpu.v[rd], es, idx, x);
            }
            0b0101 => {
                // SMOV
                let x = sext(lane(cpu.v[rn], es, idx), es) as u64;
                cpu.set(rd as u8, if q { x } else { x & 0xffff_ffff });
            }
            0b0111 => {
                // UMOV
                let x = lane(cpu.v[rn], es, idx);
                cpu.set(rd as u8, x);
            }
            _ => return false,
        }
        return true;
    }

    // ---- modified immediate: MOVI/MVNI/ORR/BIC/FMOV ----
    if bits(w, 28, 19) == 0b0_1111_0000_0 && bit(w, 10) && !bit(w, 11) {
        let cmode = bits(w, 15, 12);
        let imm8 = ((bits(w, 18, 16) << 5) | bits(w, 9, 5)) as u64;
        let op = u;
        let imm = adv_simd_expand_imm(op, cmode, imm8);
        let imm128 = (imm as u128) | ((imm as u128) << 64);
        let v = &mut cpu.v[rd];
        let is_orr_bic = (cmode & 1 == 1) && (cmode >> 2 != 3) && (cmode >> 3 == 0 || cmode >> 2 == 2);
        let r = if is_orr_bic {
            if op {
                *v & !imm128
            } else {
                *v | imm128
            }
        } else if op && cmode != 0b1110 && cmode != 0b1111 {
            !imm128 // MVNI
        } else {
            imm128
        };
        *v = trunc_q(r, q);
        return true;
    }

    // ---- EXT ----
    if bits(w, 29, 24) == 0b101110 && bits(w, 23, 21) == 0 && !bit(w, 15) && !bit(w, 10) {
        let pos = bits(w, 14, 11);
        let n = trunc_q(cpu.v[rn], q);
        let m = trunc_q(cpu.v[rm], q);
        let bytes = ds / 8;
        let mut r = 0u128;
        for i in 0..bytes {
            let src = pos + i;
            let b = if src < bytes { lane(n, 8, src) } else { lane(m, 8, src - bytes) };
            set_lane(&mut r, 8, i, b);
        }
        cpu.v[rd] = r;
        return true;
    }

    // ---- TBL/TBX ----
    if bits(w, 29, 24) == 0b001110 && bits(w, 23, 21) == 0 && !bit(w, 15) && bits(w, 11, 10) == 0 {
        let len = bits(w, 14, 13) + 1;
        let tbx = bit(w, 12);
        let idxv = cpu.v[rm];
        let mut r = if tbx { cpu.v[rd] } else { 0 };
        for i in 0..ds / 8 {
            let ix = lane(idxv, 8, i) as u32;
            if ix < len * 16 {
                let reg = (rn + (ix / 16) as usize) % 32;
                set_lane(&mut r, 8, i, lane(cpu.v[reg], 8, ix % 16));
            } else if !tbx {
                set_lane(&mut r, 8, i, 0);
            }
        }
        cpu.v[rd] = trunc_q(r, q);
        return true;
    }

    // ---- permute: UZP/TRN/ZIP ----
    if bits(w, 29, 24) == 0b001110 && !bit(w, 21) && !bit(w, 15) && bits(w, 11, 10) == 0b10 {
        let opcode = bits(w, 14, 12);
        let es = 8u32 << size;
        let el = ds / es;
        let n = cpu.v[rn];
        let m = cpu.v[rm];
        let mut r = 0u128;
        match opcode {
            0b001 | 0b101 => {
                let part = (opcode >> 2) as u32;
                for i in 0..el {
                    let src = 2 * i + part;
                    let x = if src < el { lane(n, es, src) } else { lane(m, es, src - el) };
                    set_lane(&mut r, es, i, x);
                }
            }
            0b010 | 0b110 => {
                let part = (opcode >> 2) as u32;
                for p in 0..el / 2 {
                    set_lane(&mut r, es, 2 * p, lane(n, es, 2 * p + part));
                    set_lane(&mut r, es, 2 * p + 1, lane(m, es, 2 * p + part));
                }
            }
            0b011 | 0b111 => {
                let base = if opcode == 0b111 { el / 2 } else { 0 };
                for p in 0..el / 2 {
                    set_lane(&mut r, es, 2 * p, lane(n, es, base + p));
                    set_lane(&mut r, es, 2 * p + 1, lane(m, es, base + p));
                }
            }
            _ => return false,
        }
        cpu.v[rd] = r;
        return true;
    }

    // ---- shift by immediate ----
    if bits(w, 28, 23) == 0b011110 && bit(w, 10) && bits(w, 22, 19) != 0 {
        let immh = bits(w, 22, 19);
        let immb = bits(w, 18, 16);
        let opcode = bits(w, 15, 11);
        let hb = 31 - immh.leading_zeros(); // highest set bit index of immh
        let es = 8u32 << hb;
        let immhb = (immh << 3) | immb;
        let shr = 2 * es - immhb;
        let shl = immhb - es;
        let n = cpu.v[rn];
        let mut r = 0u128;
        match (u, opcode) {
            (_, 0b00000) | (_, 0b00010) => {
                // SSHR/USHR, SSRA/USRA
                let acc = opcode == 0b00010;
                for i in 0..ds / es {
                    let x = lane(n, es, i);
                    let s = if u { if shr >= es { 0 } else { x >> shr } } else { (sext(x, es) >> shr.min(es - 1)) as u64 };
                    let v = if acc { lane(cpu.v[rd], es, i).wrapping_add(s) } else { s };
                    set_lane(&mut r, es, i, v);
                }
            }
            (false, 0b01010) => {
                for i in 0..ds / es {
                    set_lane(&mut r, es, i, lane(n, es, i) << shl);
                }
            }
            (true, 0b01010) | (true, 0b01000) => {
                // SLI / SRI
                let d = cpu.v[rd];
                for i in 0..ds / es {
                    let x = lane(n, es, i);
                    let old = lane(d, es, i);
                    let v = if opcode == 0b01010 {
                        let m = emask(es) << shl & emask(es);
                        (old & !m) | ((x << shl) & m)
                    } else {
                        let m = if shr >= es { 0 } else { emask(es) >> shr };
                        (old & !m) | (if shr >= es { 0 } else { x >> shr })
                    };
                    set_lane(&mut r, es, i, v);
                }
            }
            (_, 0b10100) => {
                // SSHLL/USHLL (UXTL/SXTL): source es, dest 2*es, from half selected by Q
                let base = if q { 64 / es } else { 0 };
                for i in 0..64 / es {
                    let x = lane(n, es, base + i);
                    let x = if u { x } else { sext(x, es) as u64 };
                    set_lane(&mut r, es * 2, i, x << shl);
                }
                cpu.v[rd] = r;
                return true;
            }
            (_, 0b10000) | (_, 0b10001) | (_, 0b10010) | (_, 0b10011) => {
                // Narrowing shifts. es = destination size, source = 2*es.
                //   U=0 10000 SHRN    10001 RSHRN    10010 SQSHRN   10011 SQRSHRN
                //   U=1 10000 SQSHRUN 10001 SQRSHRUN 10010 UQSHRN   10011 UQRSHRN
                let src_es = es * 2;
                let shr = 2 * es - immhb;
                let round = opcode & 1 == 1;
                // unsigned source: SHRN/RSHRN (bit pattern) and UQ(R)SHRN; else signed
                let unsigned_src = (!u && opcode < 0b10010) || (u && opcode >= 0b10010);
                let mut d = if q { cpu.v[rd] & (u64::MAX as u128) } else { 0 };
                let base = if q { 64 / es } else { 0 };
                for i in 0..64 / es {
                    let raw = lane(n, src_es, i);
                    let mut x: i128 = if unsigned_src { raw as i128 } else { sext(raw, src_es) as i128 };
                    if round {
                        x += 1i128 << (shr - 1);
                    }
                    let v = x >> shr;
                    let out = match (u, opcode) {
                        (false, 0b10000) | (false, 0b10001) => v as u64,
                        (true, 0b10000) | (true, 0b10001) => v.clamp(0, emask(es) as i128) as u64,
                        (false, _) => {
                            let lim = (emask(es) >> 1) as i128;
                            v.clamp(-lim - 1, lim) as u64
                        }
                        (true, _) => v.clamp(0, emask(es) as i128) as u64,
                    };
                    set_lane(&mut d, es, base + i, out & emask(es));
                }
                cpu.v[rd] = d;
                return true;
            }
            (_, 0b00100) | (_, 0b00110) => {
                // SRSHR/URSHR, SRSRA/URSRA: rounding shift right (and accumulate)
                for i in 0..ds / es {
                    let x = lane(n, es, i);
                    let x: i128 = if u { x as i128 } else { sext(x, es) as i128 };
                    let v = ((x + (1i128 << (shr - 1))) >> shr) as u64;
                    let v = if opcode == 0b00110 { lane(cpu.v[rd], es, i).wrapping_add(v) } else { v };
                    set_lane(&mut r, es, i, v);
                }
            }
            (_, 0b11100) => {
                // SCVTF/UCVTF fixed-point (vector)
                let fbits = shr;
                for i in 0..ds / es {
                    let x = lane(n, es, i);
                    let f = if u { x as f64 } else { sext(x, es) as f64 } / 2f64.powi(fbits as i32);
                    set_lane(&mut r, es, i, set_f(f, es));
                }
            }
            (_, 0b11111) => {
                // FCVTZS/FCVTZU fixed-point (vector)
                let fbits = shr;
                for i in 0..ds / es {
                    let f = lane_f(n, es, i) * 2f64.powi(fbits as i32);
                    set_lane(&mut r, es, i, fp_to_int(f, 3, !u, es == 64) & emask(es));
                }
            }
            _ => return false,
        }
        cpu.v[rd] = trunc_q(r, q);
        return true;
    }

    // ---- vector x indexed element ----
    if bits(w, 28, 24) == 0b01111 && !bit(w, 10) {
        return by_element(cpu, w, false);
    }

    if bits(w, 28, 24) != 0b01110 {
        return false;
    }

    // ---- three same ----
    if bit(w, 21) && bit(w, 10) {
        let opcode = bits(w, 15, 11);
        let n = cpu.v[rn];
        let m = cpu.v[rm];
        // bitwise ops (size encodes the op)
        if opcode == 0b00011 {
            let d = cpu.v[rd];
            let r = match (u, size) {
                (false, 0) => n & m,
                (false, 1) => n & !m,
                (false, 2) => n | m,
                (false, 3) => n | !m,
                (true, 0) => n ^ m,
                (true, 1) => (d & n) | (!d & m),         // BSL
                (true, 2) => (n & m) | (d & !m),         // BIT
                _ => (n & !m) | (d & m),                 // BIF
            };
            cpu.v[rd] = trunc_q(r, q);
            return true;
        }
        // floating point three-same: opcode 11xxx
        if opcode >= 0b11000 {
            let es = if size & 1 == 0 { 32 } else { 64 };
            let hi = size >> 1 == 1; // bit 23
            let d = cpu.v[rd];
            let mut r = 0u128;
            let el = ds / es;
            if el < 2 {
                return false; // sz=1,Q=0 is reserved
            }
            for i in 0..el {
                let a = lane_f(n, es, i);
                let b = lane_f(m, es, i);
                let x = match (u, hi, opcode) {
                    (false, false, 0b11010) => set_f(a + b, es),
                    (false, true, 0b11010) => set_f(a - b, es),
                    (true, false, 0b11011) => set_f(a * b, es),
                    (true, false, 0b11111) => set_f(a / b, es),
                    (false, false, 0b11001) => set_f(lane_f(d, es, i) + a * b, es),
                    (false, true, 0b11001) => set_f(lane_f(d, es, i) - a * b, es),
                    (false, false, 0b11110) => set_f(fmax(a, b), es),
                    (false, true, 0b11110) => set_f(fmin(a, b), es),
                    (false, false, 0b11000) => set_f(if a.is_nan() { b } else if b.is_nan() { a } else { fmax(a, b) }, es),
                    (false, true, 0b11000) => set_f(if a.is_nan() { b } else if b.is_nan() { a } else { fmin(a, b) }, es),
                    (false, false, 0b11100) => mask_cmp(a == b, es),
                    (true, false, 0b11100) => mask_cmp(a >= b, es),
                    (true, true, 0b11100) => mask_cmp(a > b, es),
                    (true, true, 0b11010) => set_f((a - b).abs(), es),
                    (false, false, 0b11111) => set_f(recip_step(a, b, es), es),
                    (false, true, 0b11111) => set_f(rsqrt_step(a, b, es), es),
                    (true, false, 0b11101) => mask_cmp(a.abs() >= b.abs(), es),
                    (true, true, 0b11101) => mask_cmp(a.abs() > b.abs(), es),
                    (true, false, 0b11010) => {
                        // FADDP
                        let src = if i < el / 2 { n } else { m };
                        let j = (i % (el / 2)) * 2;
                        set_f(lane_f(src, es, j) + lane_f(src, es, j + 1), es)
                    }
                    _ => return false,
                };
                set_lane(&mut r, es, i, x);
            }
            cpu.v[rd] = trunc_q(r, q);
            return true;
        }
        let es = 8u32 << size;
        let el = ds / es;
        if el < 2 {
            return false; // size=3,Q=0 is reserved for vector three-same
        }
        let d = cpu.v[rd];
        let mut r = 0u128;
        for i in 0..el {
            let a = lane(n, es, i);
            let b = lane(m, es, i);
            let sa = sext(a, es);
            let sb = sext(b, es);
            let pair = |i: u32| -> (u64, u64) {
                let src = if i < el / 2 { n } else { m };
                let j = (i % (el / 2)) * 2;
                (lane(src, es, j), lane(src, es, j + 1))
            };
            let x = match (u, opcode) {
                (false, 0b10000) => a.wrapping_add(b),
                (true, 0b10000) => a.wrapping_sub(b),
                (true, 0b10001) => mask_cmp(a == b, es),
                (false, 0b10001) => mask_cmp(a & b != 0, es),
                (false, 0b00110) => mask_cmp(sa > sb, es),
                (true, 0b00110) => mask_cmp(a > b, es),
                (false, 0b00111) => mask_cmp(sa >= sb, es),
                (true, 0b00111) => mask_cmp(a >= b, es),
                (false, 0b01100) => sa.max(sb) as u64,
                (true, 0b01100) => a.max(b),
                (false, 0b01101) => sa.min(sb) as u64,
                (true, 0b01101) => a.min(b),
                (false, 0b10011) => a.wrapping_mul(b),
                (false, 0b10010) => lane(d, es, i).wrapping_add(a.wrapping_mul(b)), // MLA
                (true, 0b10010) => lane(d, es, i).wrapping_sub(a.wrapping_mul(b)),  // MLS
                (false, 0b10111) => {
                    let (x, y) = pair(i);
                    x.wrapping_add(y)
                }
                (true, 0b10100) => {
                    let (x, y) = pair(i);
                    x.max(y)
                }
                (true, 0b10101) => {
                    let (x, y) = pair(i);
                    x.min(y)
                }
                (false, 0b10100) => {
                    let (x, y) = pair(i);
                    sext(x, es).max(sext(y, es)) as u64
                }
                (false, 0b10101) => {
                    let (x, y) = pair(i);
                    sext(x, es).min(sext(y, es)) as u64
                }
                (true, 0b01000) => ushl(a, sext(b & 0xff, 8), es),
                (false, 0b01000) => sshl(a, sext(b & 0xff, 8), es),
                (true, 0b00001) => {
                    // UQADD
                    let s = a as u128 + b as u128;
                    s.min(emask(es) as u128) as u64
                }
                (true, 0b00101) => a.saturating_sub(b), // UQSUB
                (false, 0b00000) => ((sa + sb) >> 1) as u64,        // SHADD
                (true, 0b00000) => ((a as u128 + b as u128) >> 1) as u64, // UHADD
                (true, 0b00010) => ((a as u128 + b as u128 + 1) >> 1) as u64, // URHADD
                (false, 0b00010) => ((sa as i128 + sb as i128 + 1) >> 1) as u64, // SRHADD
                (false, 0b00001) => {
                    let lim = (emask(es) >> 1) as i128;
                    (sa as i128 + sb as i128).clamp(-lim - 1, lim) as u64 // SQADD
                }
                (false, 0b00101) => {
                    let lim = (emask(es) >> 1) as i128;
                    (sa as i128 - sb as i128).clamp(-lim - 1, lim) as u64 // SQSUB
                }
                (false, 0b10110) | (true, 0b10110) => {
                    // SQDMULH / SQRDMULH
                    let p = 2 * sa as i128 * sb as i128 + if u { 1i128 << (es - 1) } else { 0 };
                    let lim = (emask(es) >> 1) as i128;
                    (p >> es).clamp(-lim - 1, lim) as u64
                }
                (false, 0b01001) => sat_shl_signed(sa, sext(b & 0xff, 8), es, false),
                (true, 0b01001) => sat_shl_unsigned(a, sext(b & 0xff, 8), es, false),
                (false, 0b01011) => sat_shl_signed(sa, sext(b & 0xff, 8), es, true),
                (true, 0b01011) => sat_shl_unsigned(a, sext(b & 0xff, 8), es, true),
                (false, 0b01010) => rshl_signed(sa, sext(b & 0xff, 8), es),
                (true, 0b01010) => rshl_unsigned(a, sext(b & 0xff, 8), es),
                (true, 0b01110) => (a as i128 - b as i128).unsigned_abs() as u64, // UABD
                (false, 0b01110) => (sa as i128 - sb as i128).unsigned_abs() as u64, // SABD
                (true, 0b01111) => lane(d, es, i).wrapping_add((a as i128 - b as i128).unsigned_abs() as u64), // UABA
                (false, 0b01111) => lane(d, es, i).wrapping_add((sa as i128 - sb as i128).unsigned_abs() as u64), // SABA
                _ => return false,
            };
            set_lane(&mut r, es, i, x);
        }
        cpu.v[rd] = trunc_q(r, q);
        return true;
    }

    // ---- three different (long/narrow/wide) ----
    if bit(w, 21) && bits(w, 11, 10) == 0 {
        let opcode = bits(w, 15, 12);
        let es = 8u32 << size; // source element size
        let n = cpu.v[rn];
        let m = cpu.v[rm];
        if opcode == 0b0001 || opcode == 0b0011 {
            // [SU]ADDW/[SU]SUBW{2}: Vd(2es) = Vn(2es) +- ext(Vm half)
            let base = if q { 64 / es } else { 0 };
            let mut r = 0u128;
            for i in 0..64 / es {
                let x = lane(m, es, base + i);
                let x = if u { x } else { sext(x, es) as u64 };
                let a = lane(n, es * 2, i);
                set_lane(&mut r, es * 2, i, if opcode == 0b0001 { a.wrapping_add(x) } else { a.wrapping_sub(x) });
            }
            cpu.v[rd] = r;
            return true;
        }
        if opcode == 0b0100 || opcode == 0b0110 {
            // ADDHN/SUBHN/RADDHN/RSUBHN{2}: es is the narrow (destination) size
            if size == 3 {
                return false;
            }
            let wide = es * 2;
            let mut d = if q { cpu.v[rd] & (u64::MAX as u128) } else { 0 };
            let base = if q { 64 / es } else { 0 };
            for i in 0..64 / es {
                let a = lane(n, wide, i) as u128;
                let b = lane(m, wide, i) as u128;
                let mut x = if opcode == 0b0100 { a.wrapping_add(b) } else { a.wrapping_sub(b) };
                if u {
                    x = x.wrapping_add(1u128 << (es - 1));
                }
                set_lane(&mut d, es, base + i, ((x >> es) as u64) & emask(es));
            }
            cpu.v[rd] = d;
            return true;
        }
        let base = if q { 64 / es } else { 0 };
        let mut r = 0u128;
        let d = cpu.v[rd];
        for i in 0..64 / es {
            let a = lane(n, es, base + i);
            let b = lane(m, es, base + i);
            let (a, b) = if u { (a as i128, b as i128) } else { (sext(a, es) as i128, sext(b, es) as i128) };
            let acc = lane(d, es * 2, i) as i128;
            let x = match opcode {
                0b0000 => a + b,                   // [SU]ADDL
                0b0010 => a - b,                   // [SU]SUBL
                0b1100 => a * b,                   // [SU]MULL
                0b1000 => acc + a * b,             // [SU]MLAL
                0b1010 => acc - a * b,             // [SU]MLSL
                0b0111 => (a - b).abs(),           // [SU]ABDL
                0b0101 => acc + (a - b).abs(),     // [SU]ABAL
                _ => return false,
            };
            set_lane(&mut r, es * 2, i, x as u64);
        }
        cpu.v[rd] = r;
        return true;
    }

    // ---- two-register misc ----
    if bits(w, 21, 17) == 0b10000 && bits(w, 11, 10) == 0b10 {
        let opcode = bits(w, 16, 12);
        let es = 8u32 << size;
        let el = ds / es;
        let n = cpu.v[rn];
        let mut r = 0u128;
        match (u, opcode) {
            (true, 0b10011) => {
                // SHLL/SHLL2: widen and shift left by the element size
                let base = if q { 64 / es } else { 0 };
                for i in 0..64 / es {
                    set_lane(&mut r, es * 2, i, lane(n, es, base + i) << es);
                }
                cpu.v[rd] = r;
                return true;
            }
            (true, 0b00100) if size < 3 => {
                for i in 0..el {
                    set_lane(&mut r, es, i, (lane(n, es, i).leading_zeros() - (64 - es)) as u64);
                }
            }
            (false, 0b00100) if size < 3 => {
                for i in 0..el {
                    let x = lane(n, es, i);
                    let y = x ^ ((sext(x, es) >> (es - 1)) as u64 & emask(es));
                    set_lane(&mut r, es, i, (y.leading_zeros() - (64 - es) - 1) as u64);
                }
            }
            (false, 0b00101) if size == 0 => {
                for i in 0..el {
                    set_lane(&mut r, 8, i, lane(n, 8, i).count_ones() as u64);
                }
            }
            (true, 0b00101) if size == 0 => r = !n,
            (true, 0b00101) if size == 1 => {
                for i in 0..el {
                    set_lane(&mut r, 8, i, (lane(n, 8, i) as u8).reverse_bits() as u64);
                }
            }
            (false, 0b00000) | (true, 0b00000) | (false, 0b00001) => {
                // REV64 / REV32 / REV16
                let cont = match (u, opcode) {
                    (false, 0b00000) => 64,
                    (true, 0b00000) => 32,
                    _ => 16,
                };
                if es >= cont {
                    return false;
                }
                let per = cont / es;
                for i in 0..el {
                    let blk = i / per;
                    let j = i % per;
                    set_lane(&mut r, es, blk * per + (per - 1 - j), lane(n, es, i));
                }
            }
            (false, 0b01001) => {
                for i in 0..el {
                    set_lane(&mut r, es, i, mask_cmp(lane(n, es, i) == 0, es));
                }
            }
            (false, 0b01000) => {
                for i in 0..el {
                    set_lane(&mut r, es, i, mask_cmp(sext(lane(n, es, i), es) > 0, es));
                }
            }
            (true, 0b01000) => {
                for i in 0..el {
                    set_lane(&mut r, es, i, mask_cmp(sext(lane(n, es, i), es) >= 0, es));
                }
            }
            (true, 0b01001) => {
                for i in 0..el {
                    set_lane(&mut r, es, i, mask_cmp(sext(lane(n, es, i), es) <= 0, es));
                }
            }
            (false, 0b01010) => {
                for i in 0..el {
                    set_lane(&mut r, es, i, mask_cmp(sext(lane(n, es, i), es) < 0, es));
                }
            }
            (false, 0b01011) => {
                for i in 0..el {
                    set_lane(&mut r, es, i, sext(lane(n, es, i), es).unsigned_abs());
                }
            }
            (true, 0b01011) => {
                for i in 0..el {
                    set_lane(&mut r, es, i, lane(n, es, i).wrapping_neg());
                }
            }
            (false, 0b10010) | (true, 0b10010) | (true, 0b10100) | (false, 0b10100) => {
                // XTN/XTN2, SQXTUN, UQXTN/SQXTN — es is the destination size
                let src_es = es * 2;
                let mut d = if q { cpu.v[rd] & (u64::MAX as u128) } else { 0 };
                let base = if q { 64 / es } else { 0 };
                for i in 0..64 / es {
                    let x = lane(n, src_es, i);
                    let v = match (u, opcode) {
                        (false, 0b10010) => x,
                        (true, 0b10010) => {
                            let s = sext(x, src_es);
                            s.clamp(0, emask(es) as i64) as u64
                        }
                        (true, 0b10100) => x.min(emask(es)),
                        _ => {
                            let s = sext(x, src_es);
                            let lim = (emask(es) >> 1) as i64;
                            s.clamp(-lim - 1, lim) as u64
                        }
                    };
                    set_lane(&mut d, es, base + i, v);
                }
                cpu.v[rd] = d;
                return true;
            }
            (_, 0b00010) | (_, 0b00110) => {
                // [SU]ADDLP / [SU]ADALP
                let acc = opcode == 0b00110;
                let d = cpu.v[rd];
                for i in 0..el / 2 {
                    let a = lane(n, es, 2 * i);
                    let b = lane(n, es, 2 * i + 1);
                    let s = if u { a + b } else { (sext(a, es) + sext(b, es)) as u64 };
                    let s = if acc { s.wrapping_add(lane(d, es * 2, i)) } else { s };
                    set_lane(&mut r, es * 2, i, s);
                }
            }
            _ => {
                // FP two-register misc
                let fes = if size & 1 == 0 { 32 } else { 64 };
                let hi = size >> 1 == 1;
                let fel = ds / fes;
                for i in 0..fel {
                    let x = lane(n, fes, i);
                    let f = lane_f(n, fes, i);
                    let v = match (u, hi, opcode) {
                        (false, false, 0b11101) => {
                            if fes == 32 { (sext(x, 32) as i32 as f32).to_bits() as u64 } else { (x as i64 as f64).to_bits() }
                        }
                        (true, false, 0b11101) => {
                            if fes == 32 { (x as u32 as f32).to_bits() as u64 } else { (x as f64).to_bits() }
                        }
                        (false, true, 0b11011) => fp_to_int(f, 3, true, fes == 64) & emask(fes),
                        (true, true, 0b11011) => fp_to_int(f, 3, false, fes == 64) & emask(fes),
                        (false, true, 0b01111) => set_f(f.abs(), fes),
                        (true, true, 0b01111) => set_f(-f, fes),
                        (true, true, 0b11111) => set_f(if fes == 32 { (f as f32).sqrt() as f64 } else { f.sqrt() }, fes),
                        (false, false, 0b11000) => set_f(round_mode(f, 0), fes),
                        (false, true, 0b11000) => set_f(round_mode(f, 1), fes),
                        (false, false, 0b11001) => set_f(round_mode(f, 2), fes),
                        (false, true, 0b11001) => set_f(round_mode(f, 3), fes),
                        (true, false, 0b11000) => set_f(round_mode(f, 4), fes),
                        (false, true, 0b01101) => mask_cmp(f == 0.0, fes),
                        (false, true, 0b01100) => mask_cmp(f > 0.0, fes),
                        (true, true, 0b01100) => mask_cmp(f >= 0.0, fes),
                        (false, true, 0b01110) => mask_cmp(f < 0.0, fes),
                        (true, true, 0b01101) => mask_cmp(f <= 0.0, fes),
                        (false, false, 0b11010) => fp_to_int(f, 0, true, fes == 64) & emask(fes),
                        (true, false, 0b11010) => fp_to_int(f, 0, false, fes == 64) & emask(fes),
                        (false, false, 0b11011) => fp_to_int(f, 2, true, fes == 64) & emask(fes),
                        (true, false, 0b11011) => fp_to_int(f, 2, false, fes == 64) & emask(fes),
                        (false, false, 0b11100) => fp_to_int(f, 4, true, fes == 64) & emask(fes),
                        (true, false, 0b11100) => fp_to_int(f, 4, false, fes == 64) & emask(fes),
                        (false, true, 0b11010) => fp_to_int(f, 1, true, fes == 64) & emask(fes),
                        (true, true, 0b11010) => fp_to_int(f, 1, false, fes == 64) & emask(fes),
                        (true, false, 0b11001) | (true, true, 0b11001) => set_f(round_mode(f, 0), fes),
                        (false, true, 0b11101) => recip_estimate_bits(x, fes),
                        (true, true, 0b11101) => rsqrt_estimate_bits(x, fes),
                        (false, false, 0b10111) => {
                            // FCVTL/FCVTL2: handled separately below
                            return fcvtl(cpu, w);
                        }
                        (false, false, 0b10110) => return fcvtn(cpu, w),
                        _ => return false,
                    };
                    set_lane(&mut r, fes, i, v);
                }
            }
        }
        cpu.v[rd] = trunc_q(r, q);
        return true;
    }

    // ---- across lanes ----
    if bits(w, 21, 17) == 0b11000 && bits(w, 11, 10) == 0b10 {
        let opcode = bits(w, 16, 12);
        let es = 8u32 << size;
        let el = ds / es;
        let n = cpu.v[rn];
        let it = (0..el).map(|i| lane(n, es, i));
        let r: u64 = match (u, opcode) {
            (false, 0b11011) => it.fold(0u64, |a, b| a.wrapping_add(b)) & emask(es),
            (true, 0b01010) => it.max().unwrap(),
            (true, 0b11010) => it.min().unwrap(),
            (false, 0b01010) => (0..el).map(|i| sext(lane(n, es, i), es)).max().unwrap() as u64 & emask(es),
            (false, 0b11010) => (0..el).map(|i| sext(lane(n, es, i), es)).min().unwrap() as u64 & emask(es),
            (true, 0b00011) => it.sum::<u64>() & emask(es * 2),
            (false, 0b00011) => (0..el).map(|i| sext(lane(n, es, i), es)).sum::<i64>() as u64 & emask(es * 2),
            (true, 0b01100) | (true, 0b01111) => {
                // FMAXNMV / FMAXV (single only)
                let mut m = lane_f(n, 32, 0);
                for i in 1..ds / 32 {
                    m = fmax(m, lane_f(n, 32, i));
                }
                set_f(m, 32)
            }
            _ => return false,
        };
        cpu.v[rd] = r as u128;
        return true;
    }

    false
}

/// "x indexed element" group: Vd = Vn op Vm[index] (vector or scalar form).
fn by_element(cpu: &mut Cpu, w: u32, scalar: bool) -> bool {
    let q = bit(w, 30);
    let u = bit(w, 29);
    let size = bits(w, 23, 22);
    let l = bits(w, 21, 21);
    let m = bits(w, 20, 20);
    let h = bits(w, 11, 11);
    let opcode = bits(w, 15, 12);
    let rd = bits(w, 4, 0) as usize;
    let rn = bits(w, 9, 5) as usize;
    let ds: u32 = if scalar { 0 } else if q { 128 } else { 64 };
    let is_fp = matches!((u, opcode), (false, 0b0001) | (false, 0b0101) | (false, 0b1001) | (true, 0b1001));

    let (es, index, rm) = if is_fp {
        match size {
            0b10 => (32u32, (h << 1) | l, ((m << 4) | bits(w, 19, 16)) as usize),
            0b11 => {
                if l != 0 {
                    return false;
                }
                (64, h, ((m << 4) | bits(w, 19, 16)) as usize)
            }
            _ => return false, // half precision
        }
    } else {
        match size {
            0b01 => (16u32, (h << 2) | (l << 1) | m, bits(w, 19, 16) as usize),
            0b10 => (32, (h << 1) | l, ((m << 4) | bits(w, 19, 16)) as usize),
            _ => return false,
        }
    };
    let n = cpu.v[rn];
    let d = cpu.v[rd];
    let mv = lane(cpu.v[rm], es, index);
    let el = if scalar { 1 } else { ds / es };

    if is_fp {
        let b = if es == 32 { f32::from_bits(mv as u32) as f64 } else { f64::from_bits(mv) };
        let mut r = 0u128;
        for i in 0..el {
            let a = lane_f(n, es, i);
            let acc = lane_f(d, es, i);
            let x = match (u, opcode) {
                (false, 0b1001) | (true, 0b1001) => a * b, // FMUL / FMULX
                (false, 0b0001) => {
                    if es == 32 {
                        (a as f32).mul_add(b as f32, acc as f32) as f64
                    } else {
                        a.mul_add(b, acc)
                    }
                }
                _ => {
                    if es == 32 {
                        (-(a as f32)).mul_add(b as f32, acc as f32) as f64
                    } else {
                        (-a).mul_add(b, acc)
                    }
                }
            };
            set_lane(&mut r, es, i, set_f(x, es));
        }
        cpu.v[rd] = if scalar { r & (emask(es) as u128) } else { trunc_q(r, q) };
        return true;
    }
    if scalar {
        return false; // SQDMULH etc. (scalar) not yet
    }
    let mut r = 0u128;
    match (u, opcode) {
        (false, 0b1000) | (true, 0b0000) | (true, 0b0100) => {
            // MUL / MLA / MLS
            for i in 0..el {
                let p = lane(n, es, i).wrapping_mul(mv);
                let x = match opcode {
                    0b1000 => p,
                    0b0000 => lane(d, es, i).wrapping_add(p),
                    _ => lane(d, es, i).wrapping_sub(p),
                };
                set_lane(&mut r, es, i, x);
            }
            cpu.v[rd] = trunc_q(r, q);
        }
        (_, 0b1010) | (_, 0b0010) | (_, 0b0110) => {
            // [SU]MULL / [SU]MLAL / [SU]MLSL (+2 variants use the high half)
            let base = if q { 64 / es } else { 0 };
            let b = if u { mv as i128 } else { sext(mv, es) as i128 };
            for i in 0..64 / es {
                let x = lane(n, es, base + i);
                let a = if u { x as i128 } else { sext(x, es) as i128 };
                let acc = lane(d, es * 2, i) as i128;
                let v = match opcode {
                    0b1010 => a * b,
                    0b0010 => acc + a * b,
                    _ => acc - a * b,
                };
                set_lane(&mut r, es * 2, i, v as u64);
            }
            cpu.v[rd] = r;
        }
        _ => return false,
    }
    true
}

fn fcvtl(cpu: &mut Cpu, w: u32) -> bool {
    let q = bit(w, 30);
    let sz = bit(w, 22);
    let rd = bits(w, 4, 0) as usize;
    let rn = bits(w, 9, 5) as usize;
    let n = cpu.v[rn];
    let mut r = 0u128;
    if sz {
        let base = if q { 2 } else { 0 };
        for i in 0..2 {
            set_lane(&mut r, 64, i, lane_f(n, 32, base + i).to_bits());
        }
    } else {
        let base = if q { 4 } else { 0 };
        for i in 0..4 {
            let h = f16_to_f64(lane(n, 16, base + i) as u16);
            set_lane(&mut r, 32, i, (h as f32).to_bits() as u64);
        }
    }
    cpu.v[rd] = r;
    true
}

fn fcvtn(cpu: &mut Cpu, w: u32) -> bool {
    let q = bit(w, 30);
    let sz = bit(w, 22);
    let rd = bits(w, 4, 0) as usize;
    let rn = bits(w, 9, 5) as usize;
    let n = cpu.v[rn];
    let mut d = if q { cpu.v[rd] & (u64::MAX as u128) } else { 0 };
    if sz {
        let base = if q { 2 } else { 0 };
        for i in 0..2 {
            set_lane(&mut d, 32, base + i, (lane_f(n, 64, i) as f32).to_bits() as u64);
        }
    } else {
        let base = if q { 4 } else { 0 };
        for i in 0..4 {
            set_lane(&mut d, 16, base + i, f64_to_f16(lane_f(n, 32, i)) as u64);
        }
    }
    cpu.v[rd] = d;
    true
}

/// LD1..LD4 / ST1..ST4 (multiple and single structure), LD1R..LD4R.
pub fn exec_ldst(cpu: &mut Cpu, w: u32) -> bool {
    let q = bit(w, 30);
    let l = bit(w, 22);
    let post = bit(w, 23);
    let single = bit(w, 24);
    let rt = bits(w, 4, 0) as usize;
    let rn = bits(w, 9, 5) as u8;
    let rm = bits(w, 20, 16) as u8;
    let size = bits(w, 11, 10);
    let opcode = bits(w, 15, 12);
    let base = cpu.r(if rn == 31 { 32 } else { rn });
    let mut addr = base;
    let ds: u32 = if q { 128 } else { 64 };

    let total: u64;
    unsafe {
        if !single {
            if bits(w, 21, 21) != 0 {
                return false;
            }
            let (rpt, selem) = match opcode {
                0b0000 => (1, 4),
                0b0010 => (4, 1),
                0b0100 => (1, 3),
                0b0110 => (3, 1),
                0b0111 => (1, 1),
                0b1000 => (1, 2),
                0b1010 => (2, 1),
                _ => return false,
            };
            let es = 8u32 << size;
            let eb = (es / 8) as u64;
            let el = ds / es;
            if l {
                // zero the destination registers' unused upper halves
                for r in 0..rpt * selem {
                    let t = (rt + r as usize) % 32;
                    cpu.v[t] = trunc_q(cpu.v[t], q);
                }
            }
            for r in 0..rpt {
                for e in 0..el {
                    let mut tt = (rt + r as usize) % 32;
                    for _s in 0..selem {
                        if l {
                            let x = load(addr, eb as u8);
                            set_lane(&mut cpu.v[tt], es, e, x);
                        } else {
                            store(addr, eb as u8, lane(cpu.v[tt], es, e));
                        }
                        addr += eb;
                        tt = (tt + 1) % 32;
                    }
                }
            }
            total = addr - base;
        } else {
            let r_bit = bit(w, 21) as u32;
            let op3 = bits(w, 15, 13);
            let s = bit(w, 12) as u32;
            let selem = (((op3 & 1) << 1) | r_bit) + 1;
            let mut scale = op3 >> 1;
            let index;
            match scale {
                3 => {
                    // LDnR
                    if !l {
                        return false;
                    }
                    let es = 8u32 << size;
                    let eb = (es / 8) as u8;
                    for k in 0..selem {
                        let x = load(addr, eb);
                        let t = (rt + k as usize) % 32;
                        let mut v = 0u128;
                        for i in 0..ds / es {
                            set_lane(&mut v, es, i, x);
                        }
                        cpu.v[t] = v;
                        addr += eb as u64;
                    }
                    total = addr - base;
                    if post {
                        let off = if rm == 31 { total } else { cpu.r(rm) };
                        cpu.set(if rn == 31 { 32 } else { rn }, base.wrapping_add(off));
                    }
                    return true;
                }
                0 => index = ((q as u32) << 3) | (s << 2) | size,
                1 => {
                    if size & 1 != 0 {
                        return false;
                    }
                    index = ((q as u32) << 2) | (s << 1) | (size >> 1);
                }
                _ => {
                    if size & 1 == 0 {
                        index = ((q as u32) << 1) | s;
                    } else {
                        if s != 0 || size != 1 {
                            return false;
                        }
                        index = q as u32;
                        scale = 3;
                    }
                }
            }
            let es = 8u32 << scale;
            let eb = (es / 8) as u8;
            let mut t = rt;
            for _ in 0..selem {
                if l {
                    let x = load(addr, eb);
                    set_lane(&mut cpu.v[t], es, index, x);
                } else {
                    store(addr, eb, lane(cpu.v[t], es, index));
                }
                addr += eb as u64;
                t = (t + 1) % 32;
            }
            total = addr - base;
        }
    }
    if post {
        let off = if rm == 31 { total } else { cpu.r(rm) };
        cpu.set(if rn == 31 { 32 } else { rn }, base.wrapping_add(off));
    }
    let _ = (load128 as unsafe fn(u64) -> u128, store128 as unsafe fn(u64, u128));
    true
}

// ---------------------------------------------------------------------------
// saturating / rounding shifts by register
// ---------------------------------------------------------------------------

fn sat_shl_signed(a: i64, sh: i64, es: u32, round: bool) -> u64 {
    let lim = (emask(es) >> 1) as i128;
    let a = a as i128;
    let v: i128 = if sh >= 0 {
        if sh >= 64 {
            if a == 0 { 0 } else if a > 0 { lim } else { -lim - 1 }
        } else {
            a << sh
        }
    } else {
        let s = (-sh).min(127) as u32;
        let r = if round && s <= 127 { 1i128.checked_shl(s - 1).unwrap_or(0) } else { 0 };
        if s >= 127 { if a < 0 { -1 } else { 0 } } else { (a + r) >> s }
    };
    (v.clamp(-lim - 1, lim) as u64) & emask(es)
}

fn sat_shl_unsigned(a: u64, sh: i64, es: u32, round: bool) -> u64 {
    let lim = emask(es) as u128;
    let a = (a & emask(es)) as u128;
    let v: u128 = if sh >= 0 {
        if sh >= 64 {
            if a == 0 { 0 } else { lim }
        } else {
            a << sh
        }
    } else {
        let s = (-sh).min(127) as u32;
        let r = if round { 1u128.checked_shl(s - 1).unwrap_or(0) } else { 0 };
        if s >= 127 { 0 } else { (a + r) >> s }
    };
    v.min(lim) as u64
}

fn rshl_signed(a: i64, sh: i64, es: u32) -> u64 {
    let a = a as i128;
    let v = if sh >= 0 {
        if sh >= es as i64 { 0 } else { a << sh }
    } else {
        let s = (-sh) as u32;
        if s > es { 0 } else { (a + (1i128 << (s - 1))) >> s }
    };
    (v as u64) & emask(es)
}

fn rshl_unsigned(a: u64, sh: i64, es: u32) -> u64 {
    let a = (a & emask(es)) as u128;
    let v = if sh >= 0 {
        if sh >= es as i64 { 0 } else { a << sh }
    } else {
        let s = (-sh) as u32;
        if s > es { 0 } else { (a + (1u128 << (s - 1))) >> s }
    };
    (v as u64) & emask(es)
}

// ---------------------------------------------------------------------------
// FRECPE / FRSQRTE / FRECPS / FRSQRTS  (ARM ARM pseudocode, no FEAT_RPRES)
// ---------------------------------------------------------------------------

fn recip_estimate(a: u64) -> u64 {
    // a in [256, 511]
    let a = a * 2 + 1;
    let b = (1u64 << 19) / a;
    (b + 1) / 2
}

fn rsqrt_estimate(a: u64) -> u64 {
    // a in [128, 511]
    let a = if a < 256 {
        a * 2 + 1 // 0.25 .. 0.5, units of 1/512
    } else {
        (((a >> 1) << 1) + 1) * 2 // 0.5 .. 1.0, units of 1/256
    };
    let mut b: u64 = 512;
    while a * (b + 1) * (b + 1) < (1u64 << 28) {
        b += 1;
    }
    (b + 1) / 2
}

/// Split a float into (sign, biased exp, fraction as 52-bit) with denormals
/// normalised per the ARM pseudocode (exp may go negative).
fn unpack52(x: u64, es: u32) -> (u64, i64, u64) {
    if es == 32 {
        let sign = (x >> 31) & 1;
        let mut exp = ((x >> 23) & 0xff) as i64;
        let mut frac = (x & 0x7f_ffff) << 29;
        if exp == 0 {
            while frac & (1 << 51) == 0 {
                frac = (frac << 1) & ((1 << 52) - 1);
                exp -= 1;
            }
            frac = (frac << 1) & ((1 << 52) - 1);
        }
        (sign, exp, frac)
    } else {
        let sign = x >> 63;
        let mut exp = ((x >> 52) & 0x7ff) as i64;
        let mut frac = x & ((1 << 52) - 1);
        if exp == 0 {
            while frac & (1 << 51) == 0 {
                frac = (frac << 1) & ((1 << 52) - 1);
                exp -= 1;
            }
            frac = (frac << 1) & ((1 << 52) - 1);
        }
        (sign, exp, frac)
    }
}

fn fp_special(x: u64, es: u32) -> Option<&'static str> {
    let (e, f) = if es == 32 { ((x >> 23) & 0xff, x & 0x7f_ffff) } else { ((x >> 52) & 0x7ff, x & ((1 << 52) - 1)) };
    let emax = if es == 32 { 0xff } else { 0x7ff };
    if e == emax {
        return Some(if f != 0 { "nan" } else { "inf" });
    }
    if e == 0 && f == 0 {
        return Some("zero");
    }
    None
}

fn default_nan(es: u32) -> u64 {
    if es == 32 { 0x7fc0_0000 } else { 0x7ff8_0000_0000_0000 }
}

fn inf_bits(sign: u64, es: u32) -> u64 {
    if es == 32 { (sign << 31) | 0x7f80_0000 } else { (sign << 63) | 0x7ff0_0000_0000_0000 }
}

pub fn recip_estimate_bits(x: u64, es: u32) -> u64 {
    let x = x & emask(es);
    let sign = if es == 32 { (x >> 31) & 1 } else { x >> 63 };
    match fp_special(x, es) {
        Some("nan") => return x | if es == 32 { 0x40_0000 } else { 1 << 51 },
        Some("inf") => return sign << (es - 1),
        Some("zero") => return inf_bits(sign, es),
        _ => {}
    }
    let (sign, exp, frac) = unpack52(x, es);
    let (bias2m1, fbits) = if es == 32 { (253i64, 23u32) } else { (2045i64, 52u32) };
    let scaled = (1u64 << 8) | (frac >> 44);
    let mut rexp = bias2m1 - exp;
    let est = recip_estimate(scaled);
    let mut rfrac = (est & 0xff) << 44; // 52-bit fraction
    let emax = if es == 32 { 254 } else { 2046 };
    if rexp > emax {
        return inf_bits(sign, es);
    }
    if rexp == 0 {
        rfrac = (1 << 51) | (rfrac >> 1);
    } else if rexp == -1 {
        rfrac = (1 << 50) | (rfrac >> 2);
        rexp = 0;
    } else if rexp < -1 {
        return sign << (es - 1);
    }
    if es == 32 {
        (sign << 31) | ((rexp as u64) << 23) | (rfrac >> (52 - fbits))
    } else {
        (sign << 63) | ((rexp as u64) << 52) | rfrac
    }
}

pub fn rsqrt_estimate_bits(x: u64, es: u32) -> u64 {
    let x = x & emask(es);
    let sign = if es == 32 { (x >> 31) & 1 } else { x >> 63 };
    match fp_special(x, es) {
        Some("nan") => return x | if es == 32 { 0x40_0000 } else { 1 << 51 },
        Some("zero") => return inf_bits(sign, es),
        Some("inf") if sign == 0 => return 0,
        _ => {}
    }
    if sign == 1 {
        return default_nan(es);
    }
    let (_, exp, frac) = unpack52(x, es);
    let scaled = if exp & 1 == 0 { (1u64 << 8) | (frac >> 44) } else { (1u64 << 7) | (frac >> 45) };
    let rexp = if es == 32 { (380 - exp) / 2 } else { (3068 - exp) / 2 };
    let est = rsqrt_estimate(scaled);
    if es == 32 {
        ((rexp as u64 & 0xff) << 23) | ((est & 0xff) << 15)
    } else {
        ((rexp as u64 & 0x7ff) << 52) | ((est & 0xff) << 44)
    }
}

fn is_inf_zero(a: f64, b: f64) -> bool {
    (a.is_infinite() && b == 0.0) || (a == 0.0 && b.is_infinite())
}

pub fn recip_step(a: f64, b: f64, es: u32) -> f64 {
    if is_inf_zero(a, b) {
        return 2.0;
    }
    if es == 32 {
        (-(a as f32)).mul_add(b as f32, 2.0) as f64
    } else {
        (-a).mul_add(b, 2.0)
    }
}

pub fn rsqrt_step(a: f64, b: f64, es: u32) -> f64 {
    if is_inf_zero(a, b) {
        return 1.5;
    }
    if es == 32 {
        ((-(a as f32)).mul_add(b as f32, 3.0) / 2.0) as f64
    } else {
        (-a).mul_add(b, 3.0) / 2.0
    }
}

#[cfg(test)]
mod est_tests {
    use super::*;
    #[test]
    fn estimates_close() {
        for &v in &[0.3f32, 1.0, 2.0, 7.5, 1234.5, 1e-20, 3e30] {
            let r = f32::from_bits(recip_estimate_bits(v.to_bits() as u64, 32) as u32);
            assert!(((r * v) - 1.0).abs() < 0.01, "recip {v} -> {r}");
            let s = f32::from_bits(rsqrt_estimate_bits(v.to_bits() as u64, 32) as u32);
            assert!(((s * s * v) - 1.0).abs() < 0.01, "rsqrt {v} -> {s}");
        }
        for &v in &[0.3f64, 2.0, 1e300, 5e-300] {
            let r = f64::from_bits(recip_estimate_bits(v.to_bits(), 64));
            assert!(((r * v) - 1.0).abs() < 0.01, "recip64 {v} -> {r}");
            let s = f64::from_bits(rsqrt_estimate_bits(v.to_bits(), 64));
            assert!(((s * s * v) - 1.0).abs() < 0.01, "rsqrt64 {v} -> {s}");
        }
        // known value from the ARM ARM tables: FRECPE(1.0f) = 0x3f7f8000 (0.998046875)
        assert_eq!(recip_estimate_bits(1.0f32.to_bits() as u64, 32), 0x3f7f_8000);
        // FRSQRTE(1.0f) = 0x3f7f8000? (1/sqrt(1) estimate 0.998046875)
        assert_eq!(rsqrt_estimate_bits(1.0f32.to_bits() as u64, 32), 0x3f7f_8000);
    }
}
