//! wwd-linux: a user-mode Linux/arm64 "personality" for Windows.
//!
//! Hosts an AArch64 guest process inside the current Windows process:
//! ELF loading, the initial stack/auxv, guest memory management, and
//! translation of Linux syscalls to Win32. Each guest thread is a host thread
//! running its own `wwd_cpu::Engine`.

pub mod abi;
pub mod fs;
pub mod loader;
pub mod mem;
pub mod net;
pub mod fontfix;
pub mod pipe;
pub mod props;
pub mod signal;
pub mod sys;
pub mod thunk;
pub mod util;

use abi::*;
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use wwd_cpu::{Cpu, Engine, ExitReason, Mode};

#[derive(Clone, Debug)]
pub struct Config {
    pub mode: Mode,
    pub strace: bool,
    pub stats: bool,
    pub root: std::path::PathBuf,
    pub env: Vec<String>,
    /// write a sampling profile to this host path (rewritten every 10 s)
    pub profile: Option<std::path::PathBuf>,
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

/// State shared by every thread of the guest process.
pub struct Process {
    pub cfg: Config,
    pub vfs: fs::Vfs,
    pub fds: Mutex<fs::FdTable>,
    pub brk: Mutex<mem::Brk>,
    pub cwd: Mutex<String>,
    pub sigactions: Mutex<[SigAction; 65]>,
    pub pid: u32,
    pub next_tid: AtomicU32,
    pub live_threads: AtomicU32,
    /// Bumped whenever guest code may have changed (mprotect +X, munmap...).
    pub code_gen: AtomicU64,
    pub umask: AtomicU32,
    /// sigreturn trampoline page (lazily created)
    pub sigtramp: AtomicU64,
    /// signals queued for a thread: (tid, info)
    pub pending: Mutex<Vec<(u32, signal::SigInfo)>>,
    /// live guest CPUs, for the sampling profiler
    pub cpus: Mutex<Vec<(u32, usize)>>,
    /// file-backed mappings: (start, len, file offset, guest path)
    pub file_maps: Mutex<Vec<(u64, u64, u64, String)>>,
    /// (host thread handle, cpu) for RIP sampling
    pub host_threads: Mutex<Vec<(usize, usize)>>,
}

pub struct Thread {
    pub proc: Arc<Process>,
    pub tid: u32,
    pub cpu: Box<Cpu>,
    pub engine: Engine,
    pub clear_child_tid: u64,
    pub sigmask: u64,
    pub code_gen_seen: u64,
    pub altstack: signal::AltStack,
}

thread_local! {
    static CURRENT_CPU: Cell<*const Cpu> = const { Cell::new(std::ptr::null()) };
}

impl Thread {
    pub fn new(proc: Arc<Process>, tid: u32, cpu: Box<Cpu>) -> Thread {
        let mode = proc.cfg.mode;
        let gen = proc.code_gen.load(Ordering::Acquire);
        Thread { proc, tid, cpu, engine: Engine::new(mode), clear_child_tid: 0, sigmask: 0, code_gen_seen: gen, altstack: Default::default() }
    }

    /// Run the guest thread until it exits. Returns the exit status.
    pub fn run(&mut self) -> i32 {
        CURRENT_CPU.with(|c| c.set(&*self.cpu as *const Cpu));
        let host_thread = unsafe {
            use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
            use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};
            let mut h = std::ptr::null_mut();
            DuplicateHandle(GetCurrentProcess(), GetCurrentThread(), GetCurrentProcess(), &mut h, 0, 0, DUPLICATE_SAME_ACCESS);
            h as usize
        };
        self.proc.cpus.lock().unwrap().push((host_thread as u32 as u32, &*self.cpu as *const Cpu as usize));
        self.proc.host_threads.lock().unwrap().push((host_thread, &*self.cpu as *const Cpu as usize));
        loop {
            match self.engine.run(&mut self.cpu) {
                ExitReason::Svc if self.cpu.exit_arg == thunk::SVC_THUNK => {
                    thunk::dispatch(self);
                }
                ExitReason::Svc if self.cpu.exit_arg == thunk::host::SVC_HOST => {
                    thunk::host::dispatch(self);
                }
                ExitReason::Svc => {
                    match sys::syscall(self) {
                        sys::Outcome::Ret(v) => self.cpu.x[0] = v as u64,
                        sys::Outcome::NoRet => {}
                        sys::Outcome::ExitThread(code) => return code,
                        sys::Outcome::ExitGroup(code) => exit_process(&self.proc, code),
                    }
                    signal::deliver_pending(self);
                }
                ExitReason::Fault => {
                    let addr = self.cpu.exit_arg;
                    let code = if mem::readable(addr, 1) { 2 } else { 1 }; // SEGV_ACCERR / SEGV_MAPERR
                    if self.proc.cfg.strace {
                        let w = if mem::readable(self.cpu.pc, 4) { unsafe { (self.cpu.pc as *const u32).read_unaligned() } } else { 0 };
                        eprintln!("[{}] SIGSEGV addr={addr:#x} pc={:#x} insn={w:08x} kind={}", self.tid, self.cpu.pc, self.cpu.fault_kind);
                        eprint!("{}", self.cpu.dump());
                        signal::backtrace(self);
                    }
                    signal::deliver(self, signal::SigInfo { signo: signal::SIGSEGV, code, value: addr, sync: true });
                }
                ExitReason::Breakpoint => {
                    self.cpu.bp_resume = 1; // JIT: step over it on re-entry
                    eprintln!("[wwd] breakpoint tid {} at {}", self.tid, signal::where_is(self, self.cpu.pc));
                    eprint!("{}", self.cpu.dump());
                    // show registers that point at strings (UTF-16 as UE's TCHAR, or ASCII)
                    if let Ok(regs) = std::env::var("WWD_BREAK_DUMP") {
                        // "x7,x29-0x50" style: dump UTF-16 at registers / stack slots
                        for spec0 in regs.split(',') {
                            let deref = spec0.starts_with('*');
                            let spec = spec0.trim_start_matches('*');
                            let (r, off) = spec.split_once(['-', '+']).map(|(a, b)| (a, i64::from_str_radix(b.trim_start_matches("0x"), 16).unwrap_or(0) * if spec.contains('-') { -1 } else { 1 })).unwrap_or((spec, 0));
                            let n: usize = r.trim_start_matches('x').parse().unwrap_or(0);
                            let base = if r == "sp" { self.cpu.sp() } else { self.cpu.x[n.min(32)] };
                            let mut p = mem::untag((base as i64 + off) as u64);
                            if deref && mem::readable(p, 8) {
                                p = mem::untag(unsafe { mem::read_obj::<u64>(p) });
                            }
                            if !mem::readable(p, 64) { continue; }
                            let b = unsafe { mem::slice(p, 512) };
                            let u16s: Vec<u16> = b.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
                            eprintln!("    [{spec}] @{p:#x} u\"{}\"", String::from_utf16_lossy(&u16s));
                            let q: Vec<u64> = (0..8).map(|i| unsafe { mem::read_obj::<u64>(p + i * 8) }).collect();
                            eprintln!("    [{spec}] qwords {:x?}", q);
                        }
                    }
                    for r in 0..8 {
                        let p = mem::untag(self.cpu.x[r]);
                        if p < 0x10000 || !mem::readable(p, 64) {
                            continue;
                        }
                        let b = unsafe { mem::slice(p, 256) };
                        let u16s: Vec<u16> = b.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
                        let a: Vec<u8> = b.iter().copied().take_while(|&c| c != 0).collect();
                        if u16s.len() >= 4 && u16s.iter().all(|&c| (32..0x7f).contains(&c) || c == 10) {
                            eprintln!("    x{r} -> u\"{}\"", String::from_utf16_lossy(&u16s));
                        } else if a.len() >= 4 && a.iter().all(|&c| (32..0x7f).contains(&c) || c == 10) {
                            eprintln!("    x{r} -> \"{}\"", String::from_utf8_lossy(&a));
                        }
                    }
                    signal::backtrace(self);
                }
                ExitReason::Brk => {
                    signal::deliver(self, signal::SigInfo { signo: signal::SIGTRAP, code: 1, value: self.cpu.pc, sync: true });
                }
                ExitReason::Undefined => {
                    let w = unsafe { (self.cpu.pc as *const u32).read_unaligned() };
                    eprintln!("[wwd] unimplemented/undefined instruction {:08x} at pc={:#x} (SIGILL)", w, self.cpu.pc);
                    signal::deliver(self, signal::SigInfo { signo: signal::SIGILL, code: 1, value: self.cpu.pc, sync: true });
                }
                r => {
                    eprintln!("[wwd] unexpected exit reason {r:?}");
                    exit_process(&self.proc, 1);
                }
            }
        }
    }
}

pub fn exit_process(proc: &Process, code: i32) -> ! {
    if proc.cfg.stats {
        eprintln!("[wwd] exit status {code}");
    }
    std::process::exit(code)
}

// ---------------------------------------------------------------------------
// fault reporting
// ---------------------------------------------------------------------------

unsafe extern "system" fn veh(info: *mut windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS) -> i32 {
    use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, EXCEPTION_ILLEGAL_INSTRUCTION, EXCEPTION_INT_DIVIDE_BY_ZERO};
    let rec = &*(*info).ExceptionRecord;
    let code = rec.ExceptionCode;
    if code != EXCEPTION_ACCESS_VIOLATION && code != EXCEPTION_ILLEGAL_INSTRUCTION && code != EXCEPTION_INT_DIVIDE_BY_ZERO {
        return 0; // EXCEPTION_CONTINUE_SEARCH
    }
    let cpu = CURRENT_CPU.with(|c| c.get());
    let ctx = &mut *(*info).ContextRecord;
    if code == EXCEPTION_ACCESS_VIOLATION {
        let kind = rec.ExceptionInformation[0] as u32;
        let addr = rec.ExceptionInformation[1] as u64;
        // demand paging of lazily committed guest memory
        if kind != 8 && mem::handle_fault(addr, kind == 1) {
            return -1; // EXCEPTION_CONTINUE_EXECUTION
        }
        let hc = wwd_cpu::guard::HostCtx { rip: &mut ctx.Rip, rsp: &mut ctx.Rsp };
        if wwd_cpu::guard::on_host_fault(hc, addr, kind) {
            return -1; // EXCEPTION_CONTINUE_EXECUTION
        }
    }
    let rip = ctx.Rip;
    if code == EXCEPTION_ACCESS_VIOLATION {
        let kind = match rec.ExceptionInformation[0] {
            0 => "read",
            1 => "write",
            _ => "exec",
        };
        let addr = rec.ExceptionInformation[1];
        eprintln!("[wwd] guest segmentation fault: {kind} of {addr:#x} (host rip {rip:#x})");
    } else {
        eprintln!("[wwd] host exception {code:#x} at host rip {rip:#x} ({})", host_module(rip));
    }
    if !cpu.is_null() {
        eprintln!("[wwd] guest state (pc = start of the faulting block):");
        eprint!("{}", (*cpu).dump());
    }
    std::process::exit(128 + 11);
}

pub fn install_fault_handler() {
    unsafe {
        windows_sys::Win32::System::Diagnostics::Debug::AddVectoredExceptionHandler(1, Some(veh));
    }
}

// ---------------------------------------------------------------------------
// process creation
// ---------------------------------------------------------------------------

const STACK_SIZE: u64 = 8 << 20;

fn push_bytes(sp: &mut u64, b: &[u8]) -> u64 {
    *sp -= b.len() as u64;
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), *sp as *mut u8, b.len()) };
    *sp
}

fn push_cstr(sp: &mut u64, s: &str) -> u64 {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    push_bytes(sp, &v)
}

/// Load `guest_path` (a path inside the guest root) and build the main thread.
pub fn spawn(cfg: Config, guest_path: &str, argv: &[String]) -> Result<Thread, String> {
    let vfs = fs::Vfs::new(cfg.root.clone(), fs::Vfs::normalize(guest_path, "/"));
    *vfs.cmdline.lock().unwrap() = argv.iter().flat_map(|a| a.bytes().chain(std::iter::once(0))).collect();
    let host_path = match vfs.resolve(guest_path, "/") {
        fs::Resolved::Host(h) => h,
        _ => return Err("cannot execute a special file".into()),
    };
    let data = std::fs::read(&host_path).map_err(|e| format!("{}: {e}", host_path.display()))?;
    let img = loader::load(&data)?;

    let (entry, interp_base) = match &img.interp {
        Some(ip) => {
            let fs::Resolved::Host(h) = vfs.resolve(ip, "/") else { return Err("bad interpreter".into()) };
            let idata = std::fs::read(&h).map_err(|e| format!("interpreter {}: {e}", h.display()))?;
            let iimg = loader::load(&idata)?;
            (iimg.entry, iimg.bias)
        }
        None => (img.entry, 0),
    };

    // stack
    let base = mem::map_anon(0, STACK_SIZE, mem::PROT_READ | mem::PROT_WRITE, false);
    if base < 0 {
        return Err("cannot allocate stack".into());
    }
    let top = base as u64 + STACK_SIZE;
    let mut sp = top;

    let execfn = push_cstr(&mut sp, &vfs.exe);
    let mut envp_ptrs = Vec::new();
    for e in cfg.env.iter().rev() {
        envp_ptrs.push(push_cstr(&mut sp, e));
    }
    envp_ptrs.reverse();
    let mut argv_ptrs = Vec::new();
    for a in argv.iter().rev() {
        argv_ptrs.push(push_cstr(&mut sp, a));
    }
    argv_ptrs.reverse();
    let platform = push_cstr(&mut sp, "aarch64");
    let mut rnd = [0u8; 16];
    util::random_bytes(&mut rnd);
    let random = push_bytes(&mut sp, &rnd);
    sp &= !15;

    let hwcap = HWCAP_FP | HWCAP_ASIMD | HWCAP_CRC32 | HWCAP_ATOMICS | HWCAP_CPUID;
    let auxv: Vec<(u64, u64)> = vec![
        (AT_PHDR, img.phdr),
        (AT_PHENT, 56),
        (AT_PHNUM, img.phnum),
        (AT_PAGESZ, mem::PAGE),
        (AT_BASE, interp_base),
        (AT_FLAGS, 0),
        (AT_ENTRY, img.entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_PLATFORM, platform),
        (AT_HWCAP, hwcap),
        (AT_HWCAP2, 0),
        (AT_CLKTCK, 100),
        (AT_SECURE, 0),
        (AT_RANDOM, random),
        (AT_EXECFN, execfn),
        (AT_NULL, 0),
    ];
    let words = 1 + argv_ptrs.len() + 1 + envp_ptrs.len() + 1 + auxv.len() * 2;
    sp -= (words * 8) as u64;
    sp &= !15;
    let mut p = sp;
    let mut put = |v: u64| {
        unsafe { (p as *mut u64).write(v) };
        p += 8;
    };
    put(argv_ptrs.len() as u64);
    for a in &argv_ptrs {
        put(*a);
    }
    put(0);
    for e in &envp_ptrs {
        put(*e);
    }
    put(0);
    for (k, v) in &auxv {
        put(*k);
        put(*v);
    }

    let pid = 1000 + (std::process::id() % 30000);
    let proc = Arc::new(Process {
        vfs,
        fds: Mutex::new(fs::FdTable::new_stdio()),
        brk: Mutex::new(mem::Brk::new(img.end)),
        cwd: Mutex::new("/".to_string()),
        sigactions: Mutex::new([SigAction::default(); 65]),
        pid,
        next_tid: AtomicU32::new(pid + 1),
        live_threads: AtomicU32::new(1),
        code_gen: AtomicU64::new(0),
        umask: AtomicU32::new(0o022),
        sigtramp: AtomicU64::new(0),
        pending: Mutex::new(Vec::new()),
        cpus: Mutex::new(Vec::new()),
        file_maps: Mutex::new(Vec::new()),
        host_threads: Mutex::new(Vec::new()),
        cfg,
    });
    if let Some(out) = proc.cfg.profile.clone() {
        let p = proc.clone();
        std::thread::Builder::new().name("wwd-prof".into()).spawn(move || profiler(p, out)).unwrap();
    }

    let mut cpu = Box::<Cpu>::default();
    cpu.pc = entry;
    cpu.set_sp(sp);
    Ok(Thread::new(proc, pid, cpu))
}

/// Sample every guest thread's PC once per millisecond; dump the hottest
/// locations (as library+offset) to `out` every 10 seconds.
fn profiler(p: Arc<Process>, out: std::path::PathBuf) {
    use std::collections::HashMap;
    let mut hits: HashMap<u64, u64> = HashMap::new();
    let mut total = 0u64;
    let mut last = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(1));
        let threads = p.host_threads.lock().unwrap().clone();
        for (h, c) in threads {
            let pc = unsafe { sample_pc(h, c) };
            *hits.entry(pc).or_insert(0) += 1;
            total += 1;
        }
        if last.elapsed().as_secs() >= 10 {
            last = std::time::Instant::now();
            let maps = p.file_maps.lock().unwrap().clone();
            let mut v: Vec<_> = hits.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            let mut text = format!("# {total} samples\n{}", sys::sys_report());
            for (pc, n) in v.into_iter().take(400) {
                let loc = maps
                    .iter()
                    .find(|(st, len, _, _)| (*pc & !3) >= *st && (*pc & !3) < st + len)
                    .map(|(st, _, off, path)| format!("{path}+{:#x}", (*pc & !3) - st + off))
                    .unwrap_or_else(|| "?".into());
                text += &format!("{:6} {:5.2}% {pc:#x} {loc}\n", n, *n as f64 * 100.0 / total as f64);
            }
            let _ = std::fs::write(&out, text);
        }
    }
}

#[repr(C, align(16))]
struct AlignedCtx(windows_sys::Win32::System::Diagnostics::Debug::CONTEXT);

/// Suspend a guest's host thread and translate its RIP to a guest PC.
/// Outside translated code (syscalls, interpreter fallback) the guest's
/// `cpu.pc` is current, so use that; tag syscall time with bit 0.
unsafe fn sample_pc(h: usize, cpu: usize) -> u64 {
    use windows_sys::Win32::System::Diagnostics::Debug::GetThreadContext;
    use windows_sys::Win32::System::Threading::{ResumeThread, SuspendThread};
    let h = h as windows_sys::Win32::Foundation::HANDLE;
    if SuspendThread(h) == u32::MAX {
        return 0;
    }
    let mut ctx: AlignedCtx = std::mem::zeroed();
    ctx.0.ContextFlags = 0x0010_0001; // CONTEXT_CONTROL (AMD64)
    let ok = GetThreadContext(h, &mut ctx.0) != 0;
    let cpu = &*(cpu as *const Cpu);
    let in_guest = std::ptr::read_volatile(&cpu.host_rsp) != 0;
    let pc = std::ptr::read_volatile(&cpu.pc);
    ResumeThread(h);
    if !ok {
        return 0;
    }
    match wwd_cpu::guest_pc_for_host(ctx.0.Rip) {
        Some(g) => g,
        None if in_guest => pc & !3,
        None => (pc & !3) | 1, // blocked in / handling a syscall
    }
}

/// WWD_BREAK="libfoo.so+0x1234,libfoo.so+0x5678": set interpreter breakpoints
/// once the named library's segment containing the offset is mapped.
pub fn resolve_breakpoints(path: &str, start: u64, len: u64, file_off: u64) {
    let Ok(spec) = std::env::var("WWD_BREAK") else { return };
    let name = path.rsplit('/').next().unwrap_or(path);
    for item in spec.split(',') {
        let Some((lib, off)) = item.split_once('+') else { continue };
        if lib != name {
            continue;
        }
        let Ok(o) = u64::from_str_radix(off.trim_start_matches("0x"), 16) else { continue };
        if o >= file_off && o < file_off + len {
            let addr = start + (o - file_off);
            eprintln!("[wwd] breakpoint {item} -> {addr:#x}");
            wwd_cpu::BREAKPOINTS.write().unwrap().push(addr);
        }
    }
}

/// "module.dll+0xoffset" for a host address (diagnostics).
pub fn host_module(addr: u64) -> String {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT};
    unsafe {
        let mut h = std::ptr::null_mut();
        if GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, addr as *const u16, &mut h) == 0 {
            return "?".into();
        }
        let mut buf = [0u16; 260];
        let n = GetModuleFileNameW(h, buf.as_mut_ptr(), 260) as usize;
        let name = String::from_utf16_lossy(&buf[..n]);
        format!("{}+{:#x}", name.rsplit(['\\', '/']).next().unwrap_or(&name), addr - h as u64)
    }
}
