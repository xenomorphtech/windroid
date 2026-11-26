//! AArch64 -> x86-64 dynamic binary translator.
//!
//! Translation unit: a guest basic block (up to `MAX_BLOCK` instructions,
//! ending at the first branch). Each block becomes a host function
//! `extern "sysv64" fn(*mut Cpu)` which updates `cpu.pc` and returns to the
//! dispatcher.
//!
//! Register model (deliberately simple for now):
//!   rbx  = &mut Cpu for the whole block
//!   guest registers live in `Cpu` memory; every instruction loads/stores them
//!   rax, rcx, rdx, r8-r11, xmm0-xmm1 = scratch
//! Because guest state is always in memory, a host fault inside a block
//! leaves the guest state precise (needed for SIGSEGV delivery later).
//!
//! Instructions without a native template call `helper_exec`, which runs the
//! reference interpreter on that single instruction.

use crate::cpu::*;
use crate::decode::*;
use crate::interp;
use iced_x86::code_asm::*;
use iced_x86::IcedError;
use std::mem::offset_of;
use std::sync::atomic::Ordering;

pub mod cache;

const MAX_BLOCK: usize = 200;

pub type BlockFn = unsafe extern "sysv64" fn(*mut Cpu);

const fn xoff(r: u8) -> i32 {
    (offset_of!(Cpu, x) + r as usize * 8) as i32
}
const fn voff(r: u8) -> i32 {
    (offset_of!(Cpu, v) + r as usize * 16) as i32
}
const PC: i32 = offset_of!(Cpu, pc) as i32;
const NZCV: i32 = offset_of!(Cpu, nzcv) as i32;
const TPIDR: i32 = offset_of!(Cpu, tpidr) as i32;
const EXIT_REASON: i32 = offset_of!(Cpu, exit_reason) as i32;
const EXIT_ARG: i32 = offset_of!(Cpu, exit_arg) as i32;
const CHAIN: i32 = offset_of!(Cpu, chain_slot) as i32;
const BP_RESUME: i32 = offset_of!(Cpu, bp_resume) as i32;


// ---------------------------------------------------------------------------
// helpers called from generated code
// ---------------------------------------------------------------------------

/// Execute one instruction with the interpreter. Returns 1 to continue the
/// block, 0 if the dispatcher must look at `exit_reason`.
unsafe extern "sysv64" fn helper_exec(cpu: *mut Cpu, w: u32, pc: u64) -> u32 {
    let cpu = &mut *cpu;
    cpu.pc = pc;
    interp::exec(cpu, decode(w, pc)) as u32
}

/// Every block starts with `push rbx; mov rbx, rdi` (4 bytes). Chained and
/// indirect jumps enter at the body, reusing the caller's frame.
pub const BODY_OFFSET: u64 = 4;

const LOCAL_BITS: u32 = 12;

/// Per-thread front end to the shared translation cache.
pub struct Jit {
    local: Vec<(u64, u64)>,
    gen: u64,
    has_lzcnt: bool,
}

impl Default for Jit {
    fn default() -> Self {
        Self::new()
    }
}

impl Jit {
    pub fn new() -> Jit {
        Jit { local: vec![(u64::MAX, 0); 1 << LOCAL_BITS], gen: cache::get().generation.load(Ordering::Acquire), has_lzcnt: std::arch::is_x86_feature_detected!("lzcnt") }
    }

    /// Drop every translation in the process (guest code changed).
    pub fn flush(&mut self) {
        cache::get().flush();
    }

    /// Host entry point for guest `pc`, translating it if needed.
    #[inline]
    pub fn lookup(&mut self, pc: u64) -> u64 {
        let c = cache::get();
        let g = c.generation.load(Ordering::Acquire);
        if g != self.gen {
            self.local.iter_mut().for_each(|e| *e = (u64::MAX, 0));
            self.gen = g;
        }
        let i = ((pc >> 2) as usize) & ((1 << LOCAL_BITS) - 1);
        if self.local[i].0 == pc {
            return self.local[i].1;
        }
        let hit = c.map.read().unwrap().get(&pc).copied();
        let e = match hit {
            Some(e) => e,
            None => {
                let (code, pcs, stubs) = self.translate(pc).expect("JIT assembly failed");
                c.install(pc, &code, pcs, stubs)
            }
        };
        self.local[i] = (pc, e);
        e
    }

    pub fn guest_pc_for_host(&self, rip: u64) -> Option<u64> {
        cache::get().guest_pc_for_host(rip)
    }

    /// Translate the block at `start` into position-independent code.
    /// Returns (code, [(offset, guest pc)], [(slot, stub offset)]).
    #[allow(clippy::type_complexity)]
    fn translate(&mut self, start: u64) -> Result<(Vec<u8>, Vec<(u64, u64)>, Vec<(u64, u64)>), IcedError> {
        let c = cache::get();
        let mut t = Tr { a: CodeAssembler::new(64)?, exit: CodeLabel::default(), lzcnt: self.has_lzcnt, native: 0, fallback: 0, stubs: Vec::new(), table: c.table() };
        t.exit = t.a.create_label();
        t.a.push(rbx)?;
        t.a.mov(rbx, rdi)?;
        let mut pc = start;
        let mut count = 0;
        // index of the first host instruction of each guest instruction
        let mut marks: Vec<(usize, u64)> = Vec::new();
        // debug breakpoints: a block starting on one checks the resume flag;
        // no block runs past one (see the loop)
        let bps = crate::BREAKPOINTS.read().unwrap().clone();
        if bps.contains(&start) {
            let mut go = t.a.create_label();
            t.a.cmp(dword_ptr(rbx + BP_RESUME), 0)?;
            t.a.jne(go)?;
            t.set_pc(start)?;
            t.a.mov(dword_ptr(rbx + EXIT_REASON), crate::ExitReason::Breakpoint as u32)?;
            t.a.jmp(t.exit)?;
            t.a.set_label(&mut go)?;
            t.a.mov(dword_ptr(rbx + BP_RESUME), 0)?;
        }
        loop {
            if pc != start && bps.contains(&pc) {
                // unchained exit: the dispatcher enters the breakpoint block
                t.set_pc(pc)?;
                t.a.jmp(t.exit)?;
                break;
            }
            let w = unsafe { (pc as *const u32).read_unaligned() };
            let insn = decode(w, pc);
            count += 1;
            let before = t.a.instructions().len();
            let ended = t.emit(insn, w, pc)?;
            if t.a.instructions().len() > before {
                marks.push((before, pc));
            }
            pc = pc.wrapping_add(4);
            if ended {
                break;
            }
            if count >= MAX_BLOCK {
                t.exit_direct(pc)?;
                break;
            }
        }
        t.a.set_label(&mut t.exit)?;
        t.a.pop(rbx)?;
        t.a.ret()?;
        c.stats.guest_insns.fetch_add(count as u64, Ordering::Relaxed);
        c.stats.native_insns.fetch_add(t.native, Ordering::Relaxed);
        c.stats.fallback_insns.fetch_add(t.fallback, Ordering::Relaxed);
        let res = t.a.assemble_options(0, iced_x86::BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS)?;
        let offs = &res.inner.new_instruction_offsets;
        let mut pcs = Vec::with_capacity(marks.len());
        for (idx, gpc) in &marks {
            if let Some(&o) = offs.get(*idx) {
                if o != u32::MAX {
                    pcs.push((o as u64, *gpc));
                }
            }
        }
        let mut stubs = Vec::with_capacity(t.stubs.len());
        for (slot, l) in &t.stubs {
            stubs.push((*slot, res.label_ip(l)?));
        }
        debug_assert_eq!(&res.inner.code_buffer[..4], &[0x53, 0x48, 0x89, 0xfb]);
        Ok((res.inner.code_buffer, pcs, stubs))
    }
}

struct Tr {
    a: CodeAssembler,
    exit: CodeLabel,
    lzcnt: bool,
    native: u64,
    fallback: u64,
    /// (exit slot, stub label) for chained direct exits
    stubs: Vec<(u64, CodeLabel)>,
    /// indirect-branch hash table base
    table: u64,
}

fn fits_i32(v: i64) -> bool {
    v >= i32::MIN as i64 && v <= i32::MAX as i64
}

impl Tr {
    // ---- small building blocks ----

    fn ld(&mut self, dst: AsmRegister64, r: u8) -> Result<(), IcedError> {
        self.a.mov(dst, qword_ptr(rbx + xoff(r)))
    }
    fn ld32(&mut self, dst: AsmRegister32, r: u8) -> Result<(), IcedError> {
        self.a.mov(dst, dword_ptr(rbx + xoff(r)))
    }
    /// Store a full 64-bit host register to guest register `r` (ZR discards).
    fn st(&mut self, r: u8, src: AsmRegister64) -> Result<(), IcedError> {
        if r != ZR {
            self.a.mov(qword_ptr(rbx + xoff(r)), src)?;
        }
        Ok(())
    }
    fn mov_imm(&mut self, dst: AsmRegister64, v: u64) -> Result<(), IcedError> {
        if v <= u32::MAX as u64 {
            self.a.mov(gpr32_of(dst), v as u32)
        } else {
            self.a.mov(dst, v)
        }
    }
    fn set_pc(&mut self, v: u64) -> Result<(), IcedError> {
        if v <= i32::MAX as u64 {
            self.a.mov(qword_ptr(rbx + PC), v as i32)
        } else {
            self.a.mov(rax, v)?;
            self.a.mov(qword_ptr(rbx + PC), rax)
        }
    }
    /// Store guest NZCV from the host flags of the immediately preceding
    /// arithmetic op. `sub` selects ARM carry = NOT borrow.
    fn capture_flags(&mut self, sub: bool) -> Result<(), IcedError> {
        let a = &mut self.a;
        a.sets(r8b)?;
        a.sete(r9b)?;
        if sub {
            a.setae(r10b)?;
        } else {
            a.setb(r10b)?;
        }
        a.seto(r11b)?;
        a.movzx(r8d, r8b)?;
        a.shl(r8d, 3)?;
        a.movzx(r9d, r9b)?;
        a.shl(r9d, 2)?;
        a.or(r8d, r9d)?;
        a.movzx(r10d, r10b)?;
        a.shl(r10d, 1)?;
        a.or(r8d, r10d)?;
        a.movzx(r11d, r11b)?;
        a.or(r8d, r11d)?;
        a.mov(qword_ptr(rbx + NZCV), r8)
    }
    /// Leave CF = 1 iff `cond` holds for the current guest NZCV.
    fn cond_test(&mut self, cond: u8) -> Result<(), IcedError> {
        self.a.mov(r10d, dword_ptr(rbx + NZCV))?;
        self.a.mov(r11d, COND_TABLE[cond as usize] as u32)?;
        self.a.bt(r11d, r10d)
    }
    fn jmp_exit(&mut self) -> Result<(), IcedError> {
        self.a.jmp(self.exit)
    }

    /// Leave the block for a statically known guest pc via a chainable slot.
    fn exit_direct(&mut self, target: u64) -> Result<(), IcedError> {
        let slot = cache::get().alloc_slot();
        self.a.mov(rax, slot)?;
        self.a.jmp(qword_ptr(rax))?;
        let mut stub = self.a.create_label();
        self.a.set_label(&mut stub)?;
        self.set_pc(target)?;
        self.a.mov(rax, slot)?;
        self.a.mov(qword_ptr(rbx + CHAIN), rax)?;
        self.a.jmp(self.exit)?;
        self.stubs.push((slot, stub));
        Ok(())
    }

    /// Leave the block for the guest pc in rax: probe the shared hash table
    /// and jump straight to the target's body on a hit.
    fn exit_indirect(&mut self) -> Result<(), IcedError> {
        self.a.mov(qword_ptr(rbx + PC), rax)?;
        self.a.mov(rcx, rax)?;
        self.a.shr(rcx, 2)?;
        self.a.and(ecx, (cache::TABLE_ENTRIES - 1) as u32)?;
        self.a.shl(rcx, 4)?;
        self.a.mov(rdx, self.table)?;
        self.a.add(rcx, rdx)?;
        self.a.cmp(qword_ptr(rcx), rax)?;
        self.a.jne(self.exit)?;
        self.a.jmp(qword_ptr(rcx + 8))
    }

    fn call_fallback(&mut self, w: u32, pc: u64) -> Result<(), IcedError> {
        self.fallback += 1;
        self.a.mov(rdi, rbx)?;
        self.a.mov(esi, w)?;
        self.a.mov(rdx, pc)?;
        self.a.mov(rax, helper_exec as *const () as u64)?;
        self.a.call(rax)?;
        self.a.test(eax, eax)?;
        self.a.jz(self.exit)
    }

    /// Address of a load/store into rax. Returns the writeback value plan.
    fn addr(&mut self, rn: u8, mode: AddrMode) -> Result<(), IcedError> {
        self.ld(rax, rn)?;
        match mode {
            AddrMode::Offset(i) | AddrMode::Pre(i) => {
                if i != 0 {
                    self.add_imm(rax, i)?;
                }
            }
            AddrMode::Post(_) => {}
            AddrMode::Reg { rm, ext, shift } => {
                self.ld(rcx, rm)?;
                self.extend(rcx, ext)?;
                if shift != 0 {
                    self.a.shl(rcx, shift as u32)?;
                }
                self.a.add(rax, rcx)?;
            }
        }
        if matches!(mode, AddrMode::Pre(_) | AddrMode::Post(_)) {
            self.a.mov(r10, rax)?; // tagged address, for writeback
        }
        self.untag(rax)
    }
    /// Top-Byte-Ignore: arm64 Linux ignores address bits 63:56 for data
    /// accesses (bionic/Scudo tag heap pointers); x86 would fault on them.
    fn untag(&mut self, r: AsmRegister64) -> Result<(), IcedError> {
        self.a.shl(r, 8)?;
        self.a.shr(r, 8)
    }
    fn writeback(&mut self, rn: u8, mode: AddrMode) -> Result<(), IcedError> {
        match mode {
            AddrMode::Pre(_) => self.st(rn, r10),
            AddrMode::Post(i) => {
                self.a.lea(rcx, qword_ptr(r10 + i as i32))?;
                self.st(rn, rcx)
            }
            _ => Ok(()),
        }
    }
    fn add_imm(&mut self, r: AsmRegister64, i: i64) -> Result<(), IcedError> {
        if fits_i32(i) {
            self.a.add(r, i as i32)
        } else {
            self.a.mov(r11, i as u64)?;
            self.a.add(r, r11)
        }
    }
    fn extend(&mut self, r: AsmRegister64, ext: Extend) -> Result<(), IcedError> {
        let r32 = gpr32_of(r);
        match ext {
            Extend::Uxtb => self.a.movzx(r32, gpr8_of(r)),
            Extend::Uxth => self.a.movzx(r32, gpr16_of(r)),
            Extend::Uxtw => self.a.mov(r32, r32),
            Extend::Sxtb => self.a.movsx(r, gpr8_of(r)),
            Extend::Sxth => self.a.movsx(r, gpr16_of(r)),
            Extend::Sxtw => self.a.movsxd(r, r32),
            Extend::Uxtx | Extend::Sxtx => Ok(()),
        }
    }
    fn shift(&mut self, r: AsmRegister64, sf: bool, sh: Shift, amt: u8) -> Result<(), IcedError> {
        if amt == 0 {
            return Ok(());
        }
        let a = amt as u32;
        if sf {
            match sh {
                Shift::Lsl => self.a.shl(r, a),
                Shift::Lsr => self.a.shr(r, a),
                Shift::Asr => self.a.sar(r, a),
                Shift::Ror => self.a.ror(r, a),
            }
        } else {
            let r = gpr32_of(r);
            match sh {
                Shift::Lsl => self.a.shl(r, a),
                Shift::Lsr => self.a.shr(r, a),
                Shift::Asr => self.a.sar(r, a),
                Shift::Ror => self.a.ror(r, a),
            }
        }
    }

    /// Load `op.size` bytes at [rax] into rcx with the right extension.
    fn gp_load(&mut self, op: MemOp) -> Result<(), IcedError> {
        self.gp_load_at(op, rcx, 0)
    }
    fn gp_load_at(&mut self, op: MemOp, dst: AsmRegister64, disp: i32) -> Result<(), IcedError> {
        let d32 = gpr32_of(dst);
        match (op.size, op.signed) {
            (1, false) => self.a.movzx(d32, byte_ptr(rax + disp))?,
            (2, false) => self.a.movzx(d32, word_ptr(rax + disp))?,
            (4, false) => self.a.mov(d32, dword_ptr(rax + disp))?,
            (8, _) => self.a.mov(dst, qword_ptr(rax + disp))?,
            (1, true) => self.a.movsx(dst, byte_ptr(rax + disp))?,
            (2, true) => self.a.movsx(dst, word_ptr(rax + disp))?,
            (4, true) => self.a.movsxd(dst, dword_ptr(rax + disp))?,
            _ => unreachable!(),
        }
        if op.signed && !op.dst64 {
            self.a.mov(d32, d32)?;
        }
        Ok(())
    }
    fn gp_store_at(&mut self, size: u8, src: AsmRegister64, disp: i32) -> Result<(), IcedError> {
        match size {
            1 => self.a.mov(byte_ptr(rax + disp), gpr8_of(src)),
            2 => self.a.mov(word_ptr(rax + disp), gpr16_of(src)),
            4 => self.a.mov(dword_ptr(rax + disp), gpr32_of(src)),
            _ => self.a.mov(qword_ptr(rax + disp), src),
        }
    }
    fn fp_load_at(&mut self, size: u8, vr: u8, x: AsmRegisterXmm, disp: i32) -> Result<(), IcedError> {
        match size {
            4 => self.a.movd(x, dword_ptr(rax + disp))?,
            8 => self.a.movq(x, qword_ptr(rax + disp))?,
            _ => self.a.movdqu(x, xmmword_ptr(rax + disp))?,
        }
        self.a.movdqu(xmmword_ptr(rbx + voff(vr)), x)
    }
    fn fp_store_at(&mut self, size: u8, vr: u8, disp: i32) -> Result<(), IcedError> {
        match size {
            4 => {
                self.a.mov(ecx, dword_ptr(rbx + voff(vr)))?;
                self.a.mov(dword_ptr(rax + disp), ecx)
            }
            8 => {
                self.a.mov(rcx, qword_ptr(rbx + voff(vr)))?;
                self.a.mov(qword_ptr(rax + disp), rcx)
            }
            _ => {
                self.a.movdqu(xmm0, xmmword_ptr(rbx + voff(vr)))?;
                self.a.movdqu(xmmword_ptr(rax + disp), xmm0)
            }
        }
    }

    /// Emit one guest instruction. Returns true if it ended the block.
    fn emit(&mut self, insn: Insn, w: u32, pc: u64) -> Result<bool, IcedError> {
        let next = pc.wrapping_add(4);
        let handled = self.emit_native(insn, pc, next)?;
        match handled {
            Some(ended) => {
                self.native += 1;
                Ok(ended)
            }
            None => {
                self.call_fallback(w, pc)?;
                if insn.is_branch() {
                    // helper already set pc: continue through the indirect lookup
                    self.a.mov(rax, qword_ptr(rbx + PC))?;
                    self.exit_indirect()?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
        }
    }

    /// Some(ended) if emitted natively, None to fall back to the interpreter.
    fn emit_native(&mut self, insn: Insn, _pc: u64, next: u64) -> Result<Option<bool>, IcedError> {
        match insn {
            Insn::Adr { rd, value } => {
                self.mov_imm(rax, value)?;
                self.st(rd, rax)?;
            }
            Insn::AddSubImm { sf, sub, s, rd, rn, imm } => {
                let i = imm as i32;
                if sf {
                    self.ld(rax, rn)?;
                    if sub {
                        self.a.sub(rax, i)?
                    } else {
                        self.a.add(rax, i)?
                    }
                } else {
                    self.ld32(eax, rn)?;
                    if sub {
                        self.a.sub(eax, i)?
                    } else {
                        self.a.add(eax, i)?
                    }
                }
                if s {
                    self.capture_flags(sub)?;
                }
                self.st(rd, rax)?;
            }
            Insn::AddSubShift { sf, sub, s, shift, amount, rd, rn, rm } => {
                self.ld(rax, rn)?;
                self.ld(rcx, rm)?;
                self.shift(rcx, sf, shift, amount)?;
                match (sf, sub) {
                    (true, false) => self.a.add(rax, rcx)?,
                    (true, true) => self.a.sub(rax, rcx)?,
                    (false, false) => self.a.add(eax, ecx)?,
                    (false, true) => self.a.sub(eax, ecx)?,
                }
                if s {
                    self.capture_flags(sub)?;
                }
                if !sf && !s {
                    // 32-bit op already zero-extended rax
                }
                self.st(rd, rax)?;
            }
            Insn::AddSubExt { sf, sub, s, ext, amount, rd, rn, rm } => {
                self.ld(rax, rn)?;
                self.ld(rcx, rm)?;
                self.extend(rcx, ext)?;
                if amount != 0 {
                    self.a.shl(rcx, amount as u32)?;
                }
                match (sf, sub) {
                    (true, false) => self.a.add(rax, rcx)?,
                    (true, true) => self.a.sub(rax, rcx)?,
                    (false, false) => self.a.add(eax, ecx)?,
                    (false, true) => self.a.sub(eax, ecx)?,
                }
                if s {
                    self.capture_flags(sub)?;
                }
                self.st(rd, rax)?;
            }
            Insn::LogicImm { sf, op, rd, rn, imm } => {
                self.ld(rax, rn)?;
                self.mov_imm(rcx, imm)?;
                self.logic(sf, op)?;
                self.st(rd, rax)?;
            }
            Insn::LogicReg { sf, op, invert, shift, amount, rd, rn, rm } => {
                self.ld(rax, rn)?;
                self.ld(rcx, rm)?;
                self.shift(rcx, sf, shift, amount)?;
                if invert {
                    self.a.not(rcx)?;
                }
                self.logic(sf, op)?;
                self.st(rd, rax)?;
            }
            Insn::MovWide { sf, op, rd, imm, shift } => {
                let v = imm << shift;
                let m = if sf { u64::MAX } else { 0xffff_ffff };
                match op {
                    MovOp::Z => self.mov_imm(rax, v)?,
                    MovOp::N => self.mov_imm(rax, !v & m)?,
                    MovOp::K => {
                        self.ld(rax, rd)?;
                        self.a.mov(rcx, !(0xffffu64 << shift))?;
                        self.a.and(rax, rcx)?;
                        if v != 0 {
                            self.mov_imm(rcx, v)?;
                            self.a.or(rax, rcx)?;
                        }
                        if !sf {
                            self.a.mov(eax, eax)?;
                        }
                    }
                }
                self.st(rd, rax)?;
            }
            Insn::Bitfield { sf, op, rd, rn, immr, imms } if op != BitfieldOp::Bfm => {
                let dsz = if sf { 64u32 } else { 32 };
                let r = immr as u32;
                let s = imms as u32;
                let left = dsz - 1 - s;
                let right = if s >= r { dsz - 1 - s + r } else { r - 1 - s };
                if sf {
                    self.ld(rax, rn)?;
                    if left != 0 {
                        self.a.shl(rax, left)?;
                    }
                    if right != 0 {
                        if op == BitfieldOp::Sbfm {
                            self.a.sar(rax, right)?
                        } else {
                            self.a.shr(rax, right)?
                        }
                    }
                } else {
                    self.ld32(eax, rn)?;
                    if left != 0 {
                        self.a.shl(eax, left)?;
                    }
                    if right != 0 {
                        if op == BitfieldOp::Sbfm {
                            self.a.sar(eax, right)?
                        } else {
                            self.a.shr(eax, right)?
                        }
                    }
                }
                self.st(rd, rax)?;
            }
            Insn::Extr { sf, rd, rn, rm, lsb } => {
                self.ld(rax, rm)?;
                self.ld(rcx, rn)?;
                if sf {
                    self.a.shrd(rax, rcx, lsb as u32)?;
                } else {
                    self.a.shrd(eax, ecx, lsb as u32)?;
                    self.a.mov(eax, eax)?;
                }
                self.st(rd, rax)?;
            }
            Insn::CondSel { sf, op, rd, rn, rm, cond } => {
                self.ld(rax, rn)?;
                self.ld(rcx, rm)?;
                match op {
                    CselOp::Csel => {}
                    CselOp::Csinc => self.a.inc(rcx)?,
                    CselOp::Csinv => self.a.not(rcx)?,
                    CselOp::Csneg => self.a.neg(rcx)?,
                }
                self.cond_test(cond)?;
                self.a.cmovae(rax, rcx)?;
                if !sf {
                    self.a.mov(eax, eax)?;
                }
                self.st(rd, rax)?;
            }
            Insn::CondCmp { sf, sub, rn, rm, imm, cond, nzcv } => {
                let mut skip = self.a.create_label();
                let mut done = self.a.create_label();
                self.cond_test(cond)?;
                self.a.jae(skip)?;
                self.ld(rax, rn)?;
                match imm {
                    Some(i) => self.mov_imm(rcx, i as u64)?,
                    None => self.ld(rcx, rm)?,
                }
                match (sf, sub) {
                    (true, true) => self.a.cmp(rax, rcx)?,
                    (true, false) => self.a.add(rax, rcx)?,
                    (false, true) => self.a.cmp(eax, ecx)?,
                    (false, false) => self.a.add(eax, ecx)?,
                }
                self.capture_flags(sub)?;
                self.a.jmp(done)?;
                self.a.set_label(&mut skip)?;
                self.a.mov(qword_ptr(rbx + NZCV), nzcv as i32)?;
                self.a.set_label(&mut done)?;
            }
            Insn::Dp1 { sf, op, rd, rn } => {
                match op {
                    Dp1Op::Rev => {
                        if sf {
                            self.ld(rax, rn)?;
                            self.a.bswap(rax)?;
                        } else {
                            self.ld32(eax, rn)?;
                            self.a.bswap(eax)?;
                        }
                    }
                    Dp1Op::Clz if self.lzcnt => {
                        if sf {
                            self.a.lzcnt(rax, qword_ptr(rbx + xoff(rn)))?;
                        } else {
                            self.a.lzcnt(eax, dword_ptr(rbx + xoff(rn)))?;
                        }
                    }
                    _ => return Ok(None),
                }
                self.st(rd, rax)?;
            }
            Insn::Dp2 { sf, op, rd, rn, rm } => {
                let x86op = match op {
                    Dp2Op::Lslv | Dp2Op::Lsrv | Dp2Op::Asrv | Dp2Op::Rorv => op,
                    _ => return Ok(None),
                };
                self.ld(rax, rn)?;
                self.ld(rcx, rm)?;
                if sf {
                    match x86op {
                        Dp2Op::Lslv => self.a.shl(rax, cl)?,
                        Dp2Op::Lsrv => self.a.shr(rax, cl)?,
                        Dp2Op::Asrv => self.a.sar(rax, cl)?,
                        _ => self.a.ror(rax, cl)?,
                    }
                } else {
                    match x86op {
                        Dp2Op::Lslv => self.a.shl(eax, cl)?,
                        Dp2Op::Lsrv => self.a.shr(eax, cl)?,
                        Dp2Op::Asrv => self.a.sar(eax, cl)?,
                        _ => self.a.ror(eax, cl)?,
                    }
                    self.a.mov(eax, eax)?;
                }
                self.st(rd, rax)?;
            }
            Insn::Dp3 { sf, op, rd, rn, rm, ra } => {
                match op {
                    Dp3Op::Madd | Dp3Op::Msub => {
                        self.ld(rax, rn)?;
                        self.a.imul_2(rax, qword_ptr(rbx + xoff(rm)))?;
                        if op == Dp3Op::Madd {
                            self.a.add(rax, qword_ptr(rbx + xoff(ra)))?;
                        } else {
                            self.ld(rcx, ra)?;
                            self.a.sub(rcx, rax)?;
                            self.a.mov(rax, rcx)?;
                        }
                        if !sf {
                            self.a.mov(eax, eax)?;
                        }
                    }
                    Dp3Op::Smaddl | Dp3Op::Umaddl => {
                        if op == Dp3Op::Smaddl {
                            self.a.movsxd(rax, dword_ptr(rbx + xoff(rn)))?;
                            self.a.movsxd(rcx, dword_ptr(rbx + xoff(rm)))?;
                        } else {
                            self.ld32(eax, rn)?;
                            self.ld32(ecx, rm)?;
                        }
                        self.a.imul_2(rax, rcx)?;
                        self.a.add(rax, qword_ptr(rbx + xoff(ra)))?;
                    }
                    Dp3Op::Umulh | Dp3Op::Smulh => {
                        self.ld(rax, rn)?;
                        if op == Dp3Op::Umulh {
                            self.a.mul(qword_ptr(rbx + xoff(rm)))?;
                        } else {
                            self.a.imul(qword_ptr(rbx + xoff(rm)))?;
                        }
                        self.a.mov(rax, rdx)?;
                    }
                    _ => return Ok(None),
                }
                self.st(rd, rax)?;
            }

            // ---- memory ----
            Insn::LdSt { op, rt, rn, mode } => {
                if op.prefetch {
                    return Ok(Some(false));
                }
                if op.fp && op.size < 4 {
                    return Ok(None);
                }
                self.addr(rn, mode)?;
                if op.fp {
                    if op.load {
                        self.fp_load_at(op.size, rt, xmm0, 0)?;
                    } else {
                        self.fp_store_at(op.size, rt, 0)?;
                    }
                } else if op.load {
                    self.gp_load(op)?;
                    self.st(rt, rcx)?;
                } else {
                    self.ld(rcx, rt)?;
                    self.gp_store_at(op.size, rcx, 0)?;
                }
                self.writeback(rn, mode)?;
            }
            Insn::LdStPair { op, rt, rt2, rn, mode } => {
                self.addr(rn, mode)?;
                let sz = op.size as i32;
                if op.fp {
                    if op.load {
                        self.fp_load_at(op.size, rt, xmm0, 0)?;
                        self.fp_load_at(op.size, rt2, xmm1, sz)?;
                    } else {
                        self.fp_store_at(op.size, rt, 0)?;
                        self.fp_store_at(op.size, rt2, sz)?;
                    }
                } else if op.load {
                    self.gp_load_at(op, rcx, 0)?;
                    self.gp_load_at(op, rdx, sz)?;
                    self.st(rt, rcx)?;
                    self.st(rt2, rdx)?;
                } else {
                    self.ld(rcx, rt)?;
                    self.ld(rdx, rt2)?;
                    self.gp_store_at(op.size, rcx, 0)?;
                    self.gp_store_at(op.size, rdx, sz)?;
                }
                self.writeback(rn, mode)?;
            }
            Insn::LdLit { op, rt, addr } => {
                if op.prefetch {
                    return Ok(Some(false));
                }
                self.a.mov(rax, addr)?;
                if op.fp {
                    self.fp_load_at(op.size, rt, xmm0, 0)?;
                } else {
                    self.gp_load(op)?;
                    self.st(rt, rcx)?;
                }
            }
            Insn::Ldar { size, rt, rn } => {
                self.ld(rax, rn)?;
                self.untag(rax)?;
                let op = MemOp { size, load: true, signed: false, dst64: true, fp: false, prefetch: false };
                self.gp_load(op)?;
                self.st(rt, rcx)?;
            }
            Insn::Stlr { size, rt, rn } => {
                // xchg is implicitly locked: gives the store->load ordering STLR needs
                self.ld(rax, rn)?;
                self.untag(rax)?;
                self.ld(rcx, rt)?;
                match size {
                    1 => self.a.xchg(byte_ptr(rax), cl)?,
                    2 => self.a.xchg(word_ptr(rax), cx)?,
                    4 => self.a.xchg(dword_ptr(rax), ecx)?,
                    _ => self.a.xchg(qword_ptr(rax), rcx)?,
                }
            }

            // ---- system ----
            Insn::Hint | Insn::MsrImm => {}
            Insn::Barrier { full } => {
                if full {
                    self.a.mfence()?;
                }
            }
            Insn::Mrs { rt, reg } if reg == sysregs::TPIDR_EL0 => {
                self.a.mov(rax, qword_ptr(rbx + TPIDR))?;
                self.st(rt, rax)?;
            }
            Insn::Msr { rt, reg } if reg == sysregs::TPIDR_EL0 => {
                self.ld(rax, rt)?;
                self.a.mov(qword_ptr(rbx + TPIDR), rax)?;
            }

            // ---- branches ----
            Insn::B { target, link } => {
                if link {
                    self.mov_imm(rax, next)?;
                    self.st(30, rax)?;
                }
                self.exit_direct(target)?;
                return Ok(Some(true));
            }
            Insn::BCond { cond, target } => {
                if cond >= 14 {
                    self.exit_direct(target)?;
                    return Ok(Some(true));
                }
                let mut not_taken = self.a.create_label();
                self.cond_test(cond)?;
                self.a.jae(not_taken)?;
                self.exit_direct(target)?;
                self.a.set_label(&mut not_taken)?;
                self.exit_direct(next)?;
                return Ok(Some(true));
            }
            Insn::Cbz { sf, nz, rt, target } => {
                let mut not_taken = self.a.create_label();
                if sf {
                    self.a.cmp(qword_ptr(rbx + xoff(rt)), 0)?;
                } else {
                    self.a.cmp(dword_ptr(rbx + xoff(rt)), 0)?;
                }
                if nz {
                    self.a.je(not_taken)?;
                } else {
                    self.a.jne(not_taken)?;
                }
                self.exit_direct(target)?;
                self.a.set_label(&mut not_taken)?;
                self.exit_direct(next)?;
                return Ok(Some(true));
            }
            Insn::Tbz { nz, rt, bit, target } => {
                let mut not_taken = self.a.create_label();
                self.a.bt(qword_ptr(rbx + xoff(rt)), bit as u32)?;
                if nz {
                    self.a.jae(not_taken)?;
                } else {
                    self.a.jb(not_taken)?;
                }
                self.exit_direct(target)?;
                self.a.set_label(&mut not_taken)?;
                self.exit_direct(next)?;
                return Ok(Some(true));
            }
            Insn::BrReg { rn, link } => {
                self.ld(rax, rn)?;
                if link {
                    self.mov_imm(rcx, next)?;
                    self.st(30, rcx)?;
                }
                self.exit_indirect()?;
                return Ok(Some(true));
            }
            Insn::Svc { imm } => {
                self.set_pc(next)?;
                self.a.mov(dword_ptr(rbx + EXIT_REASON), ExitReason::Svc as u32)?;
                self.a.mov(qword_ptr(rbx + EXIT_ARG), imm as i32)?;
                self.jmp_exit()?;
                return Ok(Some(true));
            }
            _ => return Ok(None),
        }
        Ok(Some(false))
    }

    fn logic(&mut self, sf: bool, op: LogicOp) -> Result<(), IcedError> {
        if sf {
            match op {
                LogicOp::And | LogicOp::Ands => self.a.and(rax, rcx)?,
                LogicOp::Orr => self.a.or(rax, rcx)?,
                LogicOp::Eor => self.a.xor(rax, rcx)?,
            }
        } else {
            match op {
                LogicOp::And | LogicOp::Ands => self.a.and(eax, ecx)?,
                LogicOp::Orr => self.a.or(eax, ecx)?,
                LogicOp::Eor => self.a.xor(eax, ecx)?,
            }
        }
        if op == LogicOp::Ands {
            // x86 AND clears CF/OF, matching ARM's C=0 V=0 for ANDS
            self.capture_flags(false)?;
        }
        Ok(())
    }
}

fn gpr32_of(r: AsmRegister64) -> AsmRegister32 {
    if r == rax {
        eax
    } else if r == rcx {
        ecx
    } else if r == rdx {
        edx
    } else if r == r8 {
        r8d
    } else if r == r9 {
        r9d
    } else if r == r10 {
        r10d
    } else if r == r11 {
        r11d
    } else {
        panic!("gpr32_of: unsupported register")
    }
}
fn gpr16_of(r: AsmRegister64) -> AsmRegister16 {
    if r == rax {
        ax
    } else if r == rcx {
        cx
    } else if r == rdx {
        dx
    } else {
        panic!("gpr16_of: unsupported register")
    }
}
fn gpr8_of(r: AsmRegister64) -> AsmRegister8 {
    if r == rax {
        al
    } else if r == rcx {
        cl
    } else if r == rdx {
        dl
    } else {
        panic!("gpr8_of: unsupported register")
    }
}
