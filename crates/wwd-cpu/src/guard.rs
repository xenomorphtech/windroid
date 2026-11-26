//! Fault containment for guest execution.
//!
//! All guest execution (JIT blocks, interpreter, translation, which reads guest
//! code) happens inside `wwd_guarded_call`, which records its stack pointer in
//! `Cpu::host_rsp` after saving every callee-saved register. When the host
//! takes an access violation inside that region, the vectored exception
//! handler calls `on_host_fault`. It fills in the guest fault state and
//! rewrites the host context to resume at `wwd_guard_resume`. That abandons
//! the faulting frames (they own no resources) and returns to the dispatcher.

use crate::cpu::{Cpu, ExitReason};
use std::cell::Cell;
use std::mem::offset_of;

core::arch::global_asm!(
    ".globl wwd_guarded_call",
    ".globl wwd_guard_resume",
    "wwd_guarded_call:",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "sub rsp, 8",
    "mov [rdi + {host_rsp}], rsp",
    "call rsi",
    "wwd_guard_resume:",
    "add rsp, 8",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "ret",
    host_rsp = const offset_of!(Cpu, host_rsp),
);

extern "sysv64" {
    fn wwd_guarded_call(cpu: *mut Cpu, f: unsafe extern "sysv64" fn(*mut Cpu));
    fn wwd_guard_resume();
}

pub type GuestFn = unsafe extern "sysv64" fn(*mut Cpu);

thread_local! {
    static CURRENT: Cell<(*mut Cpu, *const crate::jit::Jit)> = const { Cell::new((std::ptr::null_mut(), std::ptr::null())) };
}

/// Run `f(cpu)` with host faults converted into `ExitReason::Fault`.
pub fn guarded(cpu: &mut Cpu, jit: *const crate::jit::Jit, f: GuestFn) {
    CURRENT.with(|c| c.set((cpu as *mut Cpu, jit)));
    unsafe { wwd_guarded_call(cpu, f) };
    cpu.host_rsp = 0;
}

/// Register state needed from the Windows CONTEXT.
pub struct HostCtx<'a> {
    pub rip: &'a mut u64,
    pub rsp: &'a mut u64,
}

/// Called from the process's vectored exception handler for access violations.
/// Returns true if the fault belonged to guest execution and `ctx` was redirected.
pub unsafe fn on_host_fault(ctx: HostCtx, addr: u64, kind: u32) -> bool {
    let (cpu, jit) = CURRENT.with(|c| c.get());
    if cpu.is_null() || (*cpu).host_rsp == 0 {
        return false;
    }
    let cpu = &mut *cpu;
    if !jit.is_null() {
        if let Some(pc) = (*jit).guest_pc_for_host(*ctx.rip) {
            cpu.pc = pc;
        }
    }
    cpu.exit_reason = ExitReason::Fault as u32;
    cpu.exit_arg = addr;
    cpu.fault_kind = kind;
    *ctx.rsp = cpu.host_rsp;
    *ctx.rip = wwd_guard_resume as *const () as u64;
    true
}
