//! Host API thunks: guest stubs trap with `svc #0x5757` (x8 = command id),
//! and the host re-issues the call to a native Windows DLL.
//!
//! Guest memory is identity-mapped, so pointer arguments and the structs
//! behind them pass through untouched. Only the calling convention changes:
//! AAPCS64 (x0-x7, v0-v7, then stack) becomes Win64 (4 positional
//! register slots shared by int/fp, then the stack after 32 bytes of shadow space).

pub mod astc;
pub mod capture;
pub mod host;
pub mod vk_table;
pub mod vulkan;
pub mod window;

use crate::Thread;

pub const SVC_THUNK: u64 = 0x5757;

core::arch::global_asm!(
    ".globl wwd_win64_call",
    // extern "sysv64" fn(f: u64, slots: *const u64, n: u64) -> [u64; 2]  (rax, xmm0 bits)
    "wwd_win64_call:",
    "push rbp",
    "mov rbp, rsp",
    "push rbx",
    "push r12",
    "mov r10, rdi",
    "mov r11, rsi",
    "mov rbx, rdx",
    "mov rax, rbx",
    "cmp rax, 4",
    "jae 2f",
    "mov rax, 4",
    "2:",
    "lea rax, [rax*8 + 15]",
    "and rax, -16",
    "sub rsp, rax",
    "and rsp, -16",
    "mov rcx, 4",
    "3:",
    "cmp rcx, rbx",
    "jae 4f",
    "mov rax, [r11 + rcx*8]",
    "mov [rsp + rcx*8], rax",
    "inc rcx",
    "jmp 3b",
    "4:",
    "mov rcx, [r11]",
    "mov rdx, [r11 + 8]",
    "mov r8, [r11 + 16]",
    "mov r9, [r11 + 24]",
    "movq xmm0, rcx",
    "movq xmm1, rdx",
    "movq xmm2, r8",
    "movq xmm3, r9",
    "call r10",
    "movq rdx, xmm0",
    "lea rsp, [rbp - 16]",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
);

#[repr(C)]
struct Pair {
    rax: u64,
    xmm0: u64,
}

extern "sysv64" {
    fn wwd_win64_call(f: u64, slots: *const u64, n: u64) -> Pair;
}

/// Call a Win64 function. `slots[i]` is argument i (ints as-is, floats as raw bits).
pub fn win64_call(f: u64, slots: &[u64]) -> (u64, u64) {
    let mut buf = [0u64; 32];
    buf[..slots.len()].copy_from_slice(slots);
    let p = unsafe { wwd_win64_call(f, buf.as_ptr(), slots.len().max(4) as u64) };
    (p.rax, p.xmm0)
}

/// Collect `n` AAPCS64 arguments from the guest CPU; `float_mask` bit i marks
/// argument i as a scalar float/double (passed in v-registers).
pub fn guest_args(t: &Thread, n: usize, float_mask: u32) -> Vec<u64> {
    let (mut ii, mut fi, mut so) = (0usize, 0usize, 0u64);
    let sp = t.cpu.sp();
    let mut out = Vec::with_capacity(n);
    for k in 0..n {
        let is_f = float_mask & (1 << k) != 0;
        let v = if is_f && fi < 8 {
            fi += 1;
            t.cpu.v[fi - 1] as u64
        } else if !is_f && ii < 8 {
            ii += 1;
            t.cpu.x[ii - 1]
        } else {
            let v = unsafe { crate::mem::read_obj::<u64>(sp + so) };
            so += 8;
            v
        };
        out.push(v);
    }
    out
}

/// Handle `svc #0x5757`. Sets x0 (and v0) from the host call's results.
pub fn dispatch(t: &mut Thread) {
    let id = t.cpu.x[8] as usize;
    let (rax, xmm0) = vulkan::call(t, id);
    t.cpu.x[0] = rax;
    t.cpu.v[0] = xmm0 as u128;
}
