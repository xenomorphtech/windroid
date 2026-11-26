//! Guest AArch64 architectural state.
//!
//! The layout is `repr(C)` because JIT-generated code addresses fields by
//! offset (see `jit::offsets`).

/// Why execution returned to the dispatcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ExitReason {
    None = 0,
    /// SVC executed; `pc` already points past it.
    Svc = 1,
    /// BRK/HLT executed; `pc` points at the instruction.
    Brk = 2,
    /// Undefined/unimplemented instruction at `pc`. `exit_arg` holds the word.
    Undefined = 3,
    /// Guest touched code that we must re-translate (IC IVAU etc.).
    CacheFlush = 4,
    /// Host memory fault while running guest code. `exit_arg` = fault address,
    /// `fault_kind` = 0 read / 1 write / 8 exec; `pc` = faulting instruction.
    Fault = 5,
    /// Debug breakpoint hit (pc = breakpoint address, not yet executed).
    Breakpoint = 6,
}

#[repr(C, align(16))]
pub struct Cpu {
    /// X0..X30, [31] = zero register (always 0), [32] = SP.
    pub x: [u64; 33],
    pub pc: u64,
    /// NZCV in bits 3..0 (N=8, Z=4, C=2, V=1).
    pub nzcv: u64,
    pub tpidr: u64,
    pub fpcr: u64,
    pub fpsr: u64,
    /// Exclusive monitor: address (u64::MAX = cleared), value(s) observed by LDXR.
    pub excl_addr: u64,
    pub excl_val: u64,
    pub excl_val2: u64,
    pub exit_reason: u32,
    pub _pad: u32,
    pub exit_arg: u64,
    /// Host stack pointer of the guard trampoline while guest code runs (0 = not in guest).
    pub host_rsp: u64,
    pub fault_kind: u32,
    /// Set by the host to step over the breakpoint at pc once (JIT mode).
    pub bp_resume: u32,
    /// Exit slot of the direct exit just taken (0 = indirect/none): the
    /// dispatcher links it to the next block.
    pub chain_slot: u64,
    /// SIMD&FP registers V0..V31.
    pub v: [u128; 32],
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu {
            x: [0; 33],
            pc: 0,
            nzcv: 0,
            tpidr: 0,
            fpcr: 0,
            fpsr: 0,
            excl_addr: u64::MAX,
            excl_val: 0,
            excl_val2: 0,
            exit_reason: 0,
            _pad: 0,
            exit_arg: 0,
            host_rsp: 0,
            fault_kind: 0,
            bp_resume: 0,
            chain_slot: 0,
            v: [0; 32],
        }
    }
}

impl Cpu {
    #[inline(always)]
    pub fn r(&self, r: u8) -> u64 {
        self.x[r as usize]
    }

    /// 32-bit or 64-bit register read.
    #[inline(always)]
    pub fn rs(&self, r: u8, sf: bool) -> u64 {
        let v = self.x[r as usize];
        if sf {
            v
        } else {
            v & 0xffff_ffff
        }
    }

    #[inline(always)]
    pub fn set(&mut self, r: u8, v: u64) {
        if r != 31 {
            self.x[r as usize] = v;
        }
    }

    /// Write with W-register semantics (zero-extend) when !sf.
    #[inline(always)]
    pub fn sets(&mut self, r: u8, v: u64, sf: bool) {
        self.set(r, if sf { v } else { v & 0xffff_ffff });
    }

    pub fn sp(&self) -> u64 {
        self.x[32]
    }

    pub fn set_sp(&mut self, v: u64) {
        self.x[32] = v;
    }

    pub fn dump(&self) -> String {
        let mut s = String::new();
        for i in 0..31 {
            s += &format!("x{:<2} {:016x}{}", i, self.x[i], if i % 4 == 3 { "\n" } else { "  " });
        }
        s += &format!(" sp {:016x}\n pc {:016x}  nzcv {:04b}  tpidr {:016x}\n", self.x[32], self.pc, self.nzcv, self.tpidr);
        s
    }
}

pub const fn cond_eval(cond: u32, nzcv: u32) -> bool {
    let n = nzcv & 8 != 0;
    let z = nzcv & 4 != 0;
    let c = nzcv & 2 != 0;
    let v = nzcv & 1 != 0;
    let r = match cond >> 1 {
        0 => z,
        1 => c,
        2 => n,
        3 => v,
        4 => c && !z,
        5 => n == v,
        6 => n == v && !z,
        _ => true,
    };
    if cond & 1 == 1 && cond != 15 {
        !r
    } else {
        r
    }
}

const fn build_cond_table() -> [u16; 16] {
    let mut t = [0u16; 16];
    let mut c = 0;
    while c < 16 {
        let mut f = 0;
        while f < 16 {
            if cond_eval(c, f) {
                t[c as usize] |= 1 << f;
            }
            f += 1;
        }
        c += 1;
    }
    t
}

/// COND_TABLE[cond] has bit `nzcv` set iff `cond` holds for that NZCV value.
/// The JIT evaluates conditions with a single `bt` against this mask.
pub const COND_TABLE: [u16; 16] = build_cond_table();

#[inline(always)]
pub fn cond_holds(cond: u8, nzcv: u64) -> bool {
    (COND_TABLE[cond as usize] >> (nzcv & 15)) & 1 != 0
}

/// AddWithCarry() — returns (result, nzcv).
#[inline(always)]
pub fn add_with_carry(x: u64, y: u64, carry: bool, sf: bool) -> (u64, u64) {
    if sf {
        let (r1, c1) = x.overflowing_add(y);
        let (r, c2) = r1.overflowing_add(carry as u64);
        let c = c1 | c2;
        let v = ((x ^ r) & (y ^ r)) >> 63 != 0;
        let n = r >> 63 != 0;
        let z = r == 0;
        (r, flags(n, z, c, v))
    } else {
        let x = x & 0xffff_ffff;
        let y = y & 0xffff_ffff;
        let sum = x + y + carry as u64;
        let r = sum & 0xffff_ffff;
        let c = sum >> 32 != 0;
        let v = ((x ^ r) & (y ^ r)) >> 31 & 1 != 0;
        let n = r >> 31 != 0;
        let z = r == 0;
        (r, flags(n, z, c, v))
    }
}

#[inline(always)]
pub fn flags(n: bool, z: bool, c: bool, v: bool) -> u64 {
    ((n as u64) << 3) | ((z as u64) << 2) | ((c as u64) << 1) | (v as u64)
}

/// NZ flags for a logical result; C and V cleared.
#[inline(always)]
pub fn logic_flags(r: u64, sf: bool) -> u64 {
    let n = if sf { r >> 63 != 0 } else { r >> 31 & 1 != 0 };
    flags(n, r == 0, false, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conds() {
        // EQ with Z set
        assert!(cond_holds(0, 0b0100));
        assert!(!cond_holds(1, 0b0100));
        // GE: N == V
        assert!(cond_holds(10, 0b1001));
        assert!(!cond_holds(10, 0b1000));
        // AL/NV
        assert!(cond_holds(14, 0) && cond_holds(15, 0));
    }
    #[test]
    fn adc() {
        // 0 - 1 : SUBS = x + !y + 1
        let (r, f) = add_with_carry(0, !1u64, true, true);
        assert_eq!(r, u64::MAX);
        assert_eq!(f, 0b1000); // N, no carry (borrow)
        let (r, f) = add_with_carry(5, !5u64, true, true);
        assert_eq!(r, 0);
        assert_eq!(f, 0b0110); // Z, C
        let (_, f) = add_with_carry(0x7fff_ffff, 1, false, false);
        assert_eq!(f, 0b1001); // N, V
    }
}
