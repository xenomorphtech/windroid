//! Linux arm64 syscall implementations.
//!
//! Guest pointers are host pointers (identity mapping). A bad guest pointer
//! currently surfaces as a host access violation rather than EFAULT.

use crate::abi::*;
use crate::fs::{self, Node, Resolved};
use crate::mem::{self, cstr, read_obj, slice, slice_mut, write_obj};
use crate::{exit_process, Thread};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// (calls, total ns) per syscall number, for the profiler report.
pub static SYS_STATS: [(std::sync::atomic::AtomicU64, std::sync::atomic::AtomicU64); 512] =
    [const { (std::sync::atomic::AtomicU64::new(0), std::sync::atomic::AtomicU64::new(0)) }; 512];

pub fn sys_report() -> String {
    let mut v: Vec<(usize, u64, u64)> = (0..512)
        .map(|i| (i, SYS_STATS[i].0.load(std::sync::atomic::Ordering::Relaxed), SYS_STATS[i].1.load(std::sync::atomic::Ordering::Relaxed)))
        .filter(|x| x.1 > 0)
        .collect();
    v.sort_by(|a, b| b.2.cmp(&a.2));
    let mut s = String::from("# syscalls by time: nr name calls total_ms avg_us
");
    for (i, n, ns) in v.into_iter().take(25) {
        s += &format!("#   {i:3} {:18} {n:8} {:9.1} {:9.1}
", syscall_name(i as u64), ns as f64 / 1e6, ns as f64 / 1e3 / n as f64);
    }
    s
}

pub enum Outcome {
    Ret(i64),
    /// registers already set (rt_sigreturn)
    NoRet,
    ExitThread(i32),
    ExitGroup(i32),
}

fn path_arg(addr: u64) -> String {
    String::from_utf8_lossy(&unsafe { cstr(addr) }).into_owned()
}

pub fn syscall(t: &mut Thread) -> Outcome {
    let nr_ = t.cpu.x[8];
    let a = [t.cpu.x[0], t.cpu.x[1], t.cpu.x[2], t.cpu.x[3], t.cpu.x[4], t.cpu.x[5]];
    let t0 = std::time::Instant::now();
    let out = dispatch(t, nr_, a);
    let el = t0.elapsed();
    if el.as_millis() >= 20 && std::env::var_os("WWD_SLOW").is_some() {
        eprintln!("[wwd] slow syscall {} ({:?}) args {:#x} {:#x} {:#x}", syscall_name_or_nr(nr_), el, a[0], a[1], a[2]);
    }
    if (nr_ as usize) < SYS_STATS.len() {
        SYS_STATS[nr_ as usize].0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        SYS_STATS[nr_ as usize].1.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
    }
    if t.proc.cfg.strace {
        let r = match &out {
            Outcome::Ret(v) if *v < 0 && *v > -4096 => format!("-{} (errno)", -v),
            Outcome::Ret(v) => format!("{v:#x}"),
            Outcome::NoRet => "<sigreturn>".to_string(),
            Outcome::ExitThread(c) => format!("<thread exit {c}>"),
            Outcome::ExitGroup(c) => format!("<exit {c}>"),
        };
        use nr::*;
        let path = match nr_ {
            OPENAT | NEWFSTATAT | FACCESSAT | FACCESSAT2 | READLINKAT | MKDIRAT | UNLINKAT | STATX => Some(a[1]),
            CHDIR | STATFS => Some(a[0]),
            _ => None,
        };
        let ps = path.filter(|&p| p != 0 && crate::mem::readable(crate::mem::untag(p), 1)).map(|p| format!(" \"{}\"", path_arg(p))).unwrap_or_default();
        eprintln!("[{}] {}({:#x}, {:#x}, {:#x}, {:#x}){} = {}", t.tid, syscall_name_or_nr(nr_), a[0], a[1], a[2], a[3], ps, r);
    }
    out
}

fn syscall_name_or_nr(n: u64) -> String {
    match syscall_name(n) {
        "?" => format!("syscall_{n}"),
        s => s.to_string(),
    }
}

fn ok(v: i64) -> Outcome {
    Outcome::Ret(v)
}

fn dispatch(t: &mut Thread, n: u64, a: [u64; 6]) -> Outcome {
    use nr::*;
    let p = t.proc.clone();
    unsafe {
        match n {
            // ---- process / thread ----
            EXIT => {
                thread_exit(t);
                Outcome::ExitThread(a[0] as i32)
            }
            EXIT_GROUP => Outcome::ExitGroup(a[0] as i32),
            SET_TID_ADDRESS => {
                t.clear_child_tid = a[0];
                ok(t.tid as i64)
            }
            SET_ROBUST_LIST => ok(0),
            GETPID => ok(p.pid as i64),
            GETPPID => ok(1),
            GETTID => ok(t.tid as i64),
            GETUID | GETEUID | GETGID | GETEGID => ok(0),
            GETRESUID | GETRESGID => {
                for i in 0..3 {
                    if a[i] != 0 {
                        write_obj(a[i], 0u32);
                    }
                }
                ok(0)
            }
            SETUID | SETGID | SETREUID | SETREGID | SETRESUID | SETRESGID => ok(0),
            GETGROUPS => ok(0),
            GETPGID | GETSID => ok(p.pid as i64),
            SETPGID => ok(0),
            SETSID => ok(p.pid as i64),
            CLONE => ok(clone(t, a)),
            CLONE3 => ok(-ENOSYS),
            EXECVE => ok(-ENOSYS),
            WAIT4 => ok(-ECHILD),
            KILL | TKILL | TGKILL => {
                let sig = if n == TGKILL { a[2] } else { a[1] } as i32;
                let target = match n {
                    KILL => {
                        let pid = a[0] as i32;
                        if pid != p.pid as i32 && pid != 0 && pid != -1 {
                            return ok(-ESRCH);
                        }
                        t.tid
                    }
                    TKILL => a[0] as u32,
                    _ => {
                        if a[0] as u32 != p.pid {
                            return ok(-ESRCH);
                        }
                        a[1] as u32
                    }
                };
                if !(0..=64).contains(&sig) {
                    return ok(-EINVAL);
                }
                if sig == 0 {
                    return ok(0);
                }
                let info = crate::signal::SigInfo { signo: sig, code: if n == KILL { 0 } else { -6 }, value: p.pid as u64, sync: false };
                p.pending.lock().unwrap().push((target, info));
                ok(0)
            }
            139 => {
                crate::signal::sigreturn(t);
                Outcome::NoRet
            }
            RT_SIGACTION => {
                let sig = a[0] as usize;
                if sig == 0 || sig > 64 || sig == 9 || sig == 19 {
                    return ok(-EINVAL);
                }
                let mut acts = p.sigactions.lock().unwrap();
                if a[2] != 0 {
                    write_obj(a[2], acts[sig]);
                }
                if a[1] != 0 {
                    acts[sig] = read_obj(a[1]);
                }
                ok(0)
            }
            RT_SIGPROCMASK => {
                if a[2] != 0 {
                    write_obj(a[2], t.sigmask);
                }
                if a[1] != 0 {
                    let set: u64 = read_obj(a[1]);
                    match a[0] {
                        0 => t.sigmask |= set,
                        1 => t.sigmask &= !set,
                        2 => t.sigmask = set,
                        _ => return ok(-EINVAL),
                    }
                }
                ok(0)
            }
            SIGALTSTACK => {
                use crate::signal::*;
                let sp = t.cpu.sp();
                let on = t.altstack.size != 0 && sp > t.altstack.sp && sp <= t.altstack.sp + t.altstack.size;
                if a[1] != 0 {
                    let flags = if t.altstack.size == 0 || t.altstack.flags & SS_DISABLE != 0 { SS_DISABLE } else if on { SS_ONSTACK } else { 0 };
                    write_obj(a[1], t.altstack.sp);
                    write_obj(a[1] + 8, flags as u64);
                    write_obj(a[1] + 16, t.altstack.size);
                }
                if a[0] != 0 {
                    if on {
                        return ok(-EPERM);
                    }
                    let ss_sp: u64 = read_obj(a[0]);
                    let flags: u32 = read_obj(a[0] + 8);
                    let size: u64 = read_obj(a[0] + 16);
                    if flags & SS_DISABLE != 0 {
                        t.altstack = AltStack { sp: 0, size: 0, flags: SS_DISABLE };
                    } else {
                        if size < 2048 {
                            return ok(-ENOMEM);
                        }
                        t.altstack = AltStack { sp: ss_sp, size, flags: 0 };
                    }
                }
                ok(0)
            }
            PRCTL => match a[0] {
                15 => ok(0), // PR_SET_NAME
                16 => {
                    let name = b"wwd\0";
                    slice_mut(a[1], 4).copy_from_slice(name);
                    ok(0)
                }
                0x5356_4d41 => ok(0), // PR_SET_VMA (Android)
                _ => ok(-EINVAL),
            },
            SCHED_YIELD => {
                std::thread::yield_now();
                ok(0)
            }
            SCHED_GETAFFINITY => {
                let ncpu = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(64);
                let mask: u64 = if ncpu == 64 { u64::MAX } else { (1u64 << ncpu) - 1 };
                let len = (a[1] as usize).min(8);
                if len == 0 {
                    return ok(-EINVAL);
                }
                slice_mut(a[2], a[1]).fill(0);
                slice_mut(a[2], len as u64).copy_from_slice(&mask.to_le_bytes()[..len]);
                ok(8)
            }
            SCHED_SETAFFINITY | SETPRIORITY => ok(0),
            118 | 119 => ok(0),     // sched_setparam / sched_setscheduler
            120 => ok(0),           // sched_getscheduler: SCHED_OTHER
            121 => {
                // sched_getparam
                if a[1] != 0 {
                    write_obj(a[1], 0i32);
                }
                ok(0)
            }
            125 | 126 => ok(0),     // sched_get_priority_max/min for SCHED_OTHER
            240 => {
                // rt_tgsigqueueinfo(tgid, tid, sig, info): queue like tgkill, keep si_code
                let sig = a[2] as i32;
                if !(1..=64).contains(&sig) {
                    return ok(-EINVAL);
                }
                let code: i32 = if a[3] != 0 { read_obj(a[3] + 8) } else { -1 };
                let info = crate::signal::SigInfo { signo: sig, code, value: p.pid as u64, sync: false };
                p.pending.lock().unwrap().push((a[1] as u32, info));
                ok(0)
            }
            GETPRIORITY => ok(20),
            UNAME => {
                let mut buf = [0u8; 65 * 6];
                for (i, s) in ["Linux", "wwd", "6.6.0-wwd", "#1 SMP PREEMPT wwd", "aarch64", "(none)"].iter().enumerate() {
                    buf[i * 65..i * 65 + s.len()].copy_from_slice(s.as_bytes());
                }
                slice_mut(a[0], buf.len() as u64).copy_from_slice(&buf);
                ok(0)
            }
            GETRLIMIT | PRLIMIT64 => {
                let (res, old) = if n == GETRLIMIT { (a[0], a[1]) } else { (a[1], a[3]) };
                if old != 0 {
                    let v: [u64; 2] = match res {
                        3 => [8 << 20, u64::MAX], // RLIMIT_STACK
                        7 => [4096, 4096],        // RLIMIT_NOFILE
                        _ => [u64::MAX, u64::MAX],
                    };
                    write_obj(old, v);
                }
                ok(0)
            }
            SETRLIMIT => ok(0),
            GETRUSAGE => {
                slice_mut(a[1], 144).fill(0);
                ok(0)
            }
            SYSINFO => {
                let b = slice_mut(a[0], 112);
                b.fill(0);
                let up = crate::util::monotonic_ns() / 1_000_000_000;
                b[0..8].copy_from_slice(&up.to_le_bytes());
                b[32..40].copy_from_slice(&(8u64 << 30).to_le_bytes()); // totalram
                b[40..48].copy_from_slice(&(4u64 << 30).to_le_bytes()); // freeram
                b[80..82].copy_from_slice(&1u16.to_le_bytes()); // procs
                b[104..108].copy_from_slice(&1u32.to_le_bytes()); // mem_unit
                ok(0)
            }
            UMASK => ok(p.umask.swap(a[0] as u32 & 0o777, Ordering::Relaxed) as i64),
            RSEQ | MEMBARRIER => ok(-ENOSYS),

            // ---- time ----
            CLOCK_GETTIME => {
                let ts = clock_now(a[0]);
                write_obj(a[1], ts);
                ok(0)
            }
            CLOCK_GETRES => {
                if a[1] != 0 {
                    write_obj(a[1], Timespec { sec: 0, nsec: 1 });
                }
                ok(0)
            }
            GETTIMEOFDAY => {
                if a[0] != 0 {
                    let (s, ns) = crate::util::realtime();
                    write_obj(a[0], [s, ns / 1000]);
                }
                ok(0)
            }
            NANOSLEEP => {
                let ts: Timespec = read_obj(a[0]);
                sleep_ts(ts);
                ok(0)
            }
            CLOCK_NANOSLEEP => {
                let ts: Timespec = read_obj(a[2]);
                if a[1] & 1 != 0 {
                    // TIMER_ABSTIME
                    let now = clock_now(a[0]);
                    let d = (ts.sec - now.sec) as i128 * 1_000_000_000 + (ts.nsec - now.nsec) as i128;
                    if d > 0 {
                        std::thread::sleep(std::time::Duration::from_nanos(d as u64));
                    }
                } else {
                    sleep_ts(ts);
                }
                ok(0)
            }

            // ---- memory ----
            BRK => ok(p.brk.lock().unwrap().set(a[0]) as i64),
            MMAP => ok(mmap(t, a)),
            MUNMAP => {
                wwd_cpu::invalidate_code(a[0], a[1]);
                ok(mem::unmap(a[0], a[1]))
            }
            MPROTECT => {
                // code may be rewritten (or already was) once it leaves its RX state
                wwd_cpu::invalidate_code(a[0], a[1]);
                ok(mem::protect(a[0], a[1], a[2] as u32))
            }
            MREMAP => ok(mremap(t, a)),
            MADVISE => {
                if a[2] == 4 {
                    // MADV_DONTNEED: anonymous pages must read back as zero
                    mem::discard(a[0], a[1]);
                }
                ok(0)
            }
            MSYNC => ok(0),
            FUTEX => ok(futex(a)),
            GETRANDOM => {
                mem::prefault(a[0], a[1]);
                crate::util::random_bytes(slice_mut(a[0], a[1]));
                ok(a[1] as i64)
            }

            // ---- files ----
            OPENAT => ok(openat(t, a[0] as i32 as i64, a[1], a[2] as u32)),
            CLOSE => ok(p.fds.lock().unwrap().close(a[0] as i32 as i64)),
            READ => with_fd(t, a[0], |f| {
                mem::prefault(a[1], a[2]);
                fs::read(f, slice_mut(a[1], a[2]))
            }),
            WRITE => with_fd(t, a[0], |f| fs::write(f, slice(a[1], a[2]))),
            PREAD64 => with_fd(t, a[0], |f| {
                mem::prefault(a[1], a[2]);
                fs::pread(f, slice_mut(a[1], a[2]), a[3])
            }),
            PWRITE64 => with_fd(t, a[0], |f| fs::pwrite(f, slice(a[1], a[2]), a[3])),
            READV | WRITEV => with_fd(t, a[0], |f| {
                let mut total = 0i64;
                for i in 0..a[2] {
                    let iov: Iovec = read_obj(a[1] + i * 16);
                    if iov.len == 0 {
                        continue;
                    }
                    let r = if n == READV {
                        mem::prefault(iov.base, iov.len);
                        fs::read(f, slice_mut(iov.base, iov.len))
                    } else {
                        fs::write(f, slice(iov.base, iov.len))
                    };
                    if r < 0 {
                        return if total > 0 { total } else { r };
                    }
                    total += r;
                    if (r as u64) < iov.len {
                        break;
                    }
                }
                total
            }),
            LSEEK => with_fd(t, a[0], |f| fs::lseek(f, a[1] as i64, a[2] as u32)),
            FSTAT => with_fd(t, a[0], |f| match fs::fstat(f) {
                Ok(st) => {
                    write_obj(a[1], st);
                    0
                }
                Err(e) => -e,
            }),
            NEWFSTATAT => ok(fstatat(t, a[0] as i32 as i64, a[1], a[2], a[3] as u32)),
            STATX => ok(statx(t, a[0] as i32 as i64, a[1], a[2] as u32, a[4])),
            GETDENTS64 => with_fd(t, a[0], |f| fs::getdents64(f, slice_mut(a[1], a[2]))),
            IOCTL => with_fd(t, a[0], |f| ioctl(f, a[1], a[2])),
            FCNTL => ok(fcntl(t, a[0] as i32 as i64, a[1] as u32, a[2])),
            DUP => {
                let Some(f) = p.fds.lock().unwrap().get(a[0] as i32 as i64) else { return ok(-EBADF) };
                ok(p.fds.lock().unwrap().alloc(f, false, 0))
            }
            DUP3 => {
                let mut fds = p.fds.lock().unwrap();
                let Some(f) = fds.get(a[0] as i32 as i64) else { return ok(-EBADF) };
                if a[0] == a[1] {
                    return ok(-EINVAL);
                }
                fds.set_at(a[1] as usize, f, a[2] as u32 & O_CLOEXEC != 0);
                ok(a[1] as i64)
            }
            GETCWD => {
                let cwd = p.cwd.lock().unwrap().clone();
                if (cwd.len() + 1) as u64 > a[1] {
                    return ok(-ERANGE);
                }
                let b = slice_mut(a[0], cwd.len() as u64 + 1);
                b[..cwd.len()].copy_from_slice(cwd.as_bytes());
                b[cwd.len()] = 0;
                ok(cwd.len() as i64 + 1)
            }
            CHDIR => {
                let cwd = p.cwd.lock().unwrap().clone();
                let path = path_arg(a[0]);
                match p.vfs.resolve(&path, &cwd) {
                    Resolved::Host(h) if h.is_dir() => {
                        *p.cwd.lock().unwrap() = fs::Vfs::normalize(&path, &cwd);
                        ok(0)
                    }
                    Resolved::Host(h) if h.exists() => ok(-ENOTDIR),
                    _ => ok(-ENOENT),
                }
            }
            FACCESSAT | FACCESSAT2 => {
                let Some(path) = at_path(t, a[0] as i32 as i64, a[1]) else { return ok(-EBADF) };
                let cwd = p.cwd.lock().unwrap().clone();
                ok(match p.vfs.stat(&path, &cwd) {
                    Ok(_) => 0,
                    Err(e) => -e,
                })
            }
            READLINKAT => {
                let Some(path) = at_path(t, a[0] as i32 as i64, a[1]) else { return ok(-EBADF) };
                let cwd = p.cwd.lock().unwrap().clone();
                let fdlink = path.strip_prefix("/proc/self/fd/").and_then(|n| n.parse::<i64>().ok()).and_then(|fd| p.fds.lock().unwrap().get(fd)).map(|f| f.path.lock().unwrap().clone()).filter(|s| !s.is_empty());
                let target = if let Some(t) = fdlink {
                    t
                } else if let Some(t) = p.vfs.readlink(&path, &cwd) {
                    t
                } else {
                    match p.vfs.resolve(&path, &cwd) {
                        Resolved::Special("exe") => p.vfs.exe.clone(),
                        Resolved::Host(h) if h.exists() => return ok(-EINVAL),
                        _ => return ok(-ENOENT),
                    }
                };
                let n = target.len().min(a[3] as usize);
                slice_mut(a[2], n as u64).copy_from_slice(&target.as_bytes()[..n]);
                ok(n as i64)
            }
            MKDIRAT => {
                let Some(path) = at_path(t, a[0] as i32 as i64, a[1]) else { return ok(-EBADF) };
                let cwd = p.cwd.lock().unwrap().clone();
                ok(match p.vfs.resolve(&path, &cwd) {
                    Resolved::Host(h) => match std::fs::create_dir(&h) {
                        Ok(_) => 0,
                        Err(e) => -fs::io_errno(&e),
                    },
                    _ => -EEXIST,
                })
            }
            UNLINKAT => {
                let Some(path) = at_path(t, a[0] as i32 as i64, a[1]) else { return ok(-EBADF) };
                let cwd = p.cwd.lock().unwrap().clone();
                ok(match p.vfs.resolve(&path, &cwd) {
                    Resolved::Host(h) => {
                        let r = if a[2] & 0x200 != 0 { std::fs::remove_dir(&h) } else { std::fs::remove_file(&h) };
                        match r {
                            Ok(_) => 0,
                            Err(e) => -fs::io_errno(&e),
                        }
                    }
                    _ => -EACCES,
                })
            }
            RENAMEAT | 276 => {
                let (Some(from), Some(to)) = (at_path(t, a[0] as i32 as i64, a[1]), at_path(t, a[2] as i32 as i64, a[3])) else { return ok(-EBADF) };
                let cwd = p.cwd.lock().unwrap().clone();
                ok(match (p.vfs.resolve(&from, &cwd), p.vfs.resolve(&to, &cwd)) {
                    (Resolved::Host(f), Resolved::Host(tt)) => match std::fs::rename(&f, &tt) {
                        Ok(_) => 0,
                        Err(e) => -fs::io_errno(&e),
                    },
                    _ => -EACCES,
                })
            }
            FTRUNCATE => with_fd(t, a[0], |f| match &mut *f.node.lock().unwrap() {
                Node::Host { file, .. } => match file.set_len(a[1]) {
                    Ok(_) => 0,
                    Err(e) => -fs::io_errno(&e),
                },
                _ => -EINVAL,
            }),
            STATFS | FSTATFS => {
                // struct statfs (arm64, 120 bytes): report a roomy ext4-like fs
                let b = slice_mut(a[1], 120);
                b.fill(0);
                let put = |b: &mut [u8], o: usize, v: u64| b[o..o + 8].copy_from_slice(&v.to_le_bytes());
                put(b, 0, 0xef53); // f_type EXT4_SUPER_MAGIC
                put(b, 8, 4096); // f_bsize
                put(b, 16, 64 << 20); // f_blocks
                put(b, 24, 32 << 20); // f_bfree
                put(b, 32, 32 << 20); // f_bavail
                put(b, 40, 1 << 20); // f_files
                put(b, 48, 1 << 19); // f_ffree
                put(b, 64, 255); // f_namelen
                put(b, 72, 4096); // f_frsize
                ok(0)
            }
            FCHDIR => {
                let Some(d) = dir_path(t, a[0] as i32 as i64) else { return ok(-EBADF) };
                *p.cwd.lock().unwrap() = d;
                ok(0)
            }
            FSYNC | FDATASYNC | FCHMOD | FCHMODAT | FCHOWNAT => ok(0),
            5..=16 => ok(-EOPNOTSUPP), // xattr family: filesystem has no xattrs
            223 => ok(0),              // fadvise64
            PIPE2 => {
                let (r, w) = crate::pipe::new_pair();
                let fl = a[1] as u32;
                let fr = fs::open_ref(Node::Pipe(r), (fl & O_NONBLOCK) | 0);
                let fw = fs::open_ref(Node::Pipe(w), (fl & O_NONBLOCK) | O_WRONLY);
                let mut fds = p.fds.lock().unwrap();
                let a0 = fds.alloc(fr, fl & O_CLOEXEC != 0, 0);
                let a1 = fds.alloc(fw, fl & O_CLOEXEC != 0, 0);
                drop(fds);
                write_obj(a[0], [a0 as i32, a1 as i32]);
                ok(0)
            }
            PPOLL => {
                let (fds_p, nfds) = (a[0], a[1] as usize);
                let timeout = if a[2] == 0 {
                    None
                } else {
                    let ts: Timespec = read_obj(a[2]);
                    Some(std::time::Duration::new(ts.sec.max(0) as u64, ts.nsec.clamp(0, 999_999_999) as u32))
                };
                let files: Vec<(i32, i16, Option<fs::OpenRef>)> = (0..nfds)
                    .map(|i| {
                        let fd: i32 = read_obj(fds_p + i as u64 * 8);
                        let ev: i16 = read_obj(fds_p + i as u64 * 8 + 4);
                        (fd, ev, if fd >= 0 { p.fds.lock().unwrap().get(fd as i64) } else { None })
                    })
                    .collect();
                let scan = || {
                    let mut n = 0;
                    let mut out = Vec::with_capacity(files.len());
                    for (fd, ev, f) in &files {
                        let r = match f {
                            _ if *fd < 0 => 0,
                            None => crate::pipe::POLLNVAL,
                            Some(f) => fs::poll_events(f) & (*ev | crate::pipe::POLLERR | crate::pipe::POLLHUP),
                        };
                        if r != 0 {
                            n += 1;
                        }
                        out.push(r);
                    }
                    if n > 0 { Some((n, out)) } else { None }
                };
                let res = crate::pipe::wait_until(timeout, scan);
                let (n, revs) = res.unwrap_or((0, vec![0; files.len()]));
                for (i, r) in revs.iter().enumerate() {
                    write_obj(fds_p + i as u64 * 8 + 6, *r);
                }
                ok(n)
            }
            72 => {
                // pselect6(n, readfds, writefds, exceptfds, timeout, sigmask)
                let n = (a[0] as usize).min(1024);
                let sets = [a[1], a[2], a[3]];
                let timeout = if a[4] == 0 {
                    None
                } else {
                    let ts: Timespec = read_obj(a[4]);
                    Some(std::time::Duration::new(ts.sec.max(0) as u64, ts.nsec.clamp(0, 999_999_999) as u32))
                };
                let words = n.div_ceil(64);
                let mut want = [vec![0u64; words], vec![0u64; words], vec![0u64; words]];
                for (k, &sp) in sets.iter().enumerate() {
                    if sp != 0 {
                        for w in 0..words {
                            want[k][w] = read_obj(sp + w as u64 * 8);
                        }
                    }
                }
                let scan = || {
                    let mut out = [vec![0u64; words], vec![0u64; words], vec![0u64; words]];
                    let mut count = 0i64;
                    for fd in 0..n {
                        let (w, b) = (fd / 64, 1u64 << (fd % 64));
                        if (want[0][w] | want[1][w] | want[2][w]) & b == 0 {
                            continue;
                        }
                        let ev = match p.fds.lock().unwrap().get(fd as i64) {
                            Some(f) => fs::poll_events(&f),
                            None => crate::pipe::POLLNVAL,
                        };
                        if want[0][w] & b != 0 && ev & (crate::pipe::POLLIN | crate::pipe::POLLHUP) != 0 {
                            out[0][w] |= b;
                            count += 1;
                        }
                        if want[1][w] & b != 0 && ev & crate::pipe::POLLOUT != 0 {
                            out[1][w] |= b;
                            count += 1;
                        }
                    }
                    if count > 0 { Some((count, out)) } else { None }
                };
                let (cnt, out) = crate::pipe::wait_until(timeout, scan).unwrap_or((0, [vec![0u64; words], vec![0u64; words], vec![0u64; words]]));
                for (k, &sp) in sets.iter().enumerate() {
                    if sp != 0 {
                        for w in 0..words {
                            write_obj(sp + w as u64 * 8, out[k][w]);
                        }
                    }
                }
                ok(cnt)
            }
            SOCKET => {
                let (fam, ty) = (a[0] as u32, a[1] as u32);
                if fam == crate::net::LINUX_AF_UNIX {
                    return ok(-crate::net::EAFNOSUPPORT);
                }
                match crate::net::socket(fam, ty, a[2] as u32) {
                    Ok(s) => {
                        let fl = if ty & crate::net::SOCK_NONBLOCK != 0 { O_NONBLOCK | O_RDWR } else { O_RDWR };
                        let f = fs::open_ref(Node::Socket(s), fl);
                        ok(p.fds.lock().unwrap().alloc(f, ty & crate::net::SOCK_CLOEXEC != 0, 0))
                    }
                    Err(e) => ok(-e),
                }
            }
            200 | 203 => {
                // bind / connect
                with_sock(t, a[0], |s| {
                    let sa = match crate::net::sockaddr_in(a[1], a[2] as u32) {
                        Ok(b) => b,
                        Err(e) => return -e,
                    };
                    let r = if n == 200 {
                        windows_sys::Win32::Networking::WinSock::bind(s.s, sa.as_ptr() as *const _, sa.len() as i32)
                    } else {
                        windows_sys::Win32::Networking::WinSock::connect(s.s, sa.as_ptr() as *const _, sa.len() as i32)
                    };
                    let r = crate::net::ret(r);
                    // a non-blocking connect in progress is EINPROGRESS on Linux
                    if n == 203 && r == -EAGAIN { -crate::net::EINPROGRESS } else { r }
                })
            }
            201 => with_sock(t, a[0], |s| crate::net::ret(windows_sys::Win32::Networking::WinSock::listen(s.s, a[1] as i32))),
            202 | 242 => {
                // accept / accept4
                let file = p.fds.lock().unwrap().get(a[0] as i32 as i64);
                let Some(file) = file else { return ok(-EBADF) };
                let mut buf = [0u8; 128];
                let mut len: i32 = 128;
                let ns = {
                    let n = file.node.lock().unwrap();
                    let Node::Socket(s) = &*n else { return ok(-ENOTSOCK) };
                    windows_sys::Win32::Networking::WinSock::accept(s.s, buf.as_mut_ptr() as *mut _, &mut len)
                };
                if ns == windows_sys::Win32::Networking::WinSock::INVALID_SOCKET {
                    return ok(-crate::net::last_errno());
                }
                crate::net::sockaddr_out(&buf[..len as usize], a[1], a[2]);
                let fl4 = if n == 242 { a[3] as u32 } else { 0 };
                let s = crate::net::Socket { s: ns, family: crate::net::LINUX_AF_INET, nonblock: fl4 & crate::net::SOCK_NONBLOCK != 0 };
                crate::net::set_nonblock(&s, s.nonblock);
                let f = fs::open_ref(Node::Socket(s), O_RDWR | (fl4 & O_NONBLOCK));
                ok(p.fds.lock().unwrap().alloc(f, fl4 & crate::net::SOCK_CLOEXEC != 0, 0))
            }
            204 | 205 => with_sock(t, a[0], |s| {
                let mut buf = [0u8; 128];
                let mut len: i32 = 128;
                let r = if n == 204 {
                    windows_sys::Win32::Networking::WinSock::getsockname(s.s, buf.as_mut_ptr() as *mut _, &mut len)
                } else {
                    windows_sys::Win32::Networking::WinSock::getpeername(s.s, buf.as_mut_ptr() as *mut _, &mut len)
                };
                if r == 0 {
                    crate::net::sockaddr_out(&buf[..len as usize], a[1], a[2]);
                }
                crate::net::ret(r)
            }),
            206 => with_sock(t, a[0], |s| {
                // sendto(fd, buf, len, flags, addr, addrlen)
                mem::prefault(a[1], a[2]);
                let data = slice(a[1], a[2]);
                if std::env::var_os("WWD_TRACE_NET").is_some() {
                    eprintln!("[wwd] net sendto {} bytes: {}", data.len(), String::from_utf8_lossy(&data[..data.len().min(2000)]));
                }
                let r = if a[4] != 0 {
                    match crate::net::sockaddr_in(a[4], a[5] as u32) {
                        Ok(sa) => windows_sys::Win32::Networking::WinSock::sendto(s.s, data.as_ptr(), data.len() as i32, 0, sa.as_ptr() as *const _, sa.len() as i32),
                        Err(e) => return -e,
                    }
                } else {
                    windows_sys::Win32::Networking::WinSock::send(s.s, data.as_ptr(), data.len() as i32, 0)
                };
                crate::net::ret(r)
            }),
            207 => with_sock(t, a[0], |s| {
                // recvfrom(fd, buf, len, flags, addr, addrlen)
                mem::prefault(a[1], a[2]);
                let data = slice_mut(a[1], a[2]);
                let flags = if a[3] & 2 != 0 { 2 } else { 0 }; // MSG_PEEK
                if a[3] & 0x40 != 0 && crate::net::readiness(s) & crate::pipe::POLLIN == 0 {
                    return -EAGAIN; // MSG_DONTWAIT
                }
                let mut buf = [0u8; 128];
                let mut len: i32 = 128;
                let r = windows_sys::Win32::Networking::WinSock::recvfrom(s.s, data.as_mut_ptr(), data.len() as i32, flags, buf.as_mut_ptr() as *mut _, &mut len);
                if r >= 0 && a[4] != 0 {
                    crate::net::sockaddr_out(&buf[..len.max(0) as usize], a[4], a[5]);
                }
                crate::net::ret(r)
            }),
            208 | 209 => with_sock(t, a[0], |s| {
                let Some((lvl, name)) = crate::net::map_opt(a[1] as i32, a[2] as i32) else {
                    if n == 209 && a[3] != 0 {
                        write_obj(a[3], 0u32);
                        if a[4] != 0 {
                            write_obj(a[4], 4u32);
                        }
                    }
                    return 0;
                };
                let is_timeo = a[1] == 1 && (a[2] == 20 || a[2] == 21);
                if n == 208 {
                    if is_timeo {
                        // Linux timeval -> Windows DWORD milliseconds
                        let tv: [i64; 2] = read_obj(a[3]);
                        let ms: u32 = (tv[0] * 1000 + tv[1] / 1000).max(0) as u32;
                        return crate::net::ret(windows_sys::Win32::Networking::WinSock::setsockopt(s.s, lvl, name, &ms as *const u32 as *const u8, 4));
                    }
                    let v = slice(a[3], a[4]);
                    crate::net::ret(windows_sys::Win32::Networking::WinSock::setsockopt(s.s, lvl, name, v.as_ptr(), v.len() as i32))
                } else {
                    let mut buf = [0u8; 16];
                    let mut len: i32 = 16;
                    let r = windows_sys::Win32::Networking::WinSock::getsockopt(s.s, lvl, name, buf.as_mut_ptr(), &mut len);
                    if r == 0 {
                        let mut v = u32::from_le_bytes(buf[0..4].try_into().unwrap());
                        if a[1] == 1 && a[2] == 4 && v != 0 {
                            // SO_ERROR holds a WSA code: translate to errno
                            windows_sys::Win32::Networking::WinSock::WSASetLastError(v as i32);
                            v = crate::net::last_errno() as u32;
                        }
                        write_obj(a[3], v);
                        write_obj(a[4], 4u32);
                    }
                    crate::net::ret(r)
                }
            }),
            210 => with_sock(t, a[0], |s| crate::net::ret(windows_sys::Win32::Networking::WinSock::shutdown(s.s, a[1] as i32))),
            211 | 212 => with_sock(t, a[0], |s| {
                // sendmsg / recvmsg: msghdr { name, namelen, iov, iovlen, control, controllen, flags }
                let (name, namelen, iov, iovlen): (u64, u32, u64, u64) = (read_obj(a[1]), read_obj(a[1] + 8), read_obj(a[1] + 16), read_obj(a[1] + 24));
                let mut total = 0i64;
                for i in 0..iovlen {
                    let v: Iovec = read_obj(iov + i * 16);
                    if v.len == 0 {
                        continue;
                    }
                    mem::prefault(v.base, v.len);
                    let r = if n == 211 {
                        let d = slice(v.base, v.len);
                        if name != 0 && total == 0 {
                            match crate::net::sockaddr_in(name, namelen) {
                                Ok(sa) => windows_sys::Win32::Networking::WinSock::sendto(s.s, d.as_ptr(), d.len() as i32, 0, sa.as_ptr() as *const _, sa.len() as i32),
                                Err(e) => return -e,
                            }
                        } else {
                            windows_sys::Win32::Networking::WinSock::send(s.s, d.as_ptr(), d.len() as i32, 0)
                        }
                    } else {
                        let d = slice_mut(v.base, v.len);
                        windows_sys::Win32::Networking::WinSock::recv(s.s, d.as_mut_ptr(), d.len() as i32, 0)
                    };
                    if r < 0 {
                        return if total > 0 { total } else { -crate::net::last_errno() };
                    }
                    total += r as i64;
                    if (r as u64) < v.len {
                        break;
                    }
                }
                if n == 212 {
                    write_obj(a[1] + 40, 0u64); // no control data
                    write_obj(a[1] + 48, 0u32);
                }
                total
            }),
            EVENTFD2 => {
                // eventfd2(initval, flags): EFD_SEMAPHORE 1, EFD_NONBLOCK 0o4000, EFD_CLOEXEC 0o2000000
                let fl = a[1] as u32;
                let f = fs::open_ref(Node::EventFd(crate::pipe::eventfd(a[0] as u32 as u64, fl & 1 != 0)), O_RDWR | (fl & O_NONBLOCK));
                ok(p.fds.lock().unwrap().alloc(f, fl & 0o2000000 != 0, 0))
            }
            232 => {
                // mincore: report every page resident
                let pages = (a[1] + 4095) / 4096;
                mem::prefault(a[2], pages);
                for i in 0..pages {
                    write_obj(a[2] + i, 1u8);
                }
                ok(0)
            }
            EPOLL_CREATE1 => ok(-ENOSYS),

            _ => {
                eprintln!("[wwd] unimplemented syscall {} ({}) at pc={:#x}", n, syscall_name(n), t.cpu.pc);
                ok(-ENOSYS)
            }
        }
    }
}

fn with_sock(t: &Thread, fd: u64, f: impl FnOnce(&crate::net::Socket) -> i64) -> Outcome {
    let file = t.proc.fds.lock().unwrap().get(fd as i32 as i64);
    let Some(file) = file else { return Outcome::Ret(-EBADF) };
    let n = file.node.lock().unwrap();
    match &*n {
        Node::Socket(s) => {
            // clone the handle so long blocking calls don't hold the node lock
            let s2 = crate::net::Socket { s: s.s, family: s.family, nonblock: s.nonblock };
            drop(n);
            let r = f(&s2);
            std::mem::forget(s2);
            Outcome::Ret(r)
        }
        _ => Outcome::Ret(-ENOTSOCK),
    }
}

fn with_fd(t: &Thread, fd: u64, f: impl FnOnce(&fs::Open) -> i64) -> Outcome {
    let file = t.proc.fds.lock().unwrap().get(fd as i32 as i64);
    match file {
        Some(file) => Outcome::Ret(f(&file)),
        None => Outcome::Ret(-EBADF),
    }
}

/// Guest path of a directory fd, for *at() syscalls.
fn dir_path(t: &Thread, dirfd: i64) -> Option<String> {
    if dirfd == AT_FDCWD {
        return Some(t.proc.cwd.lock().unwrap().clone());
    }
    let f = t.proc.fds.lock().unwrap().get(dirfd)?;
    let n = f.node.lock().unwrap();
    match &*n {
        Node::Dir { path, .. } | Node::Host { path, .. } => {
            let rel = path.strip_prefix(&t.proc.vfs.root).ok()?;
            let s: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            Some(format!("/{}", s.join("/")))
        }
        _ => None,
    }
}

/// Resolve a (dirfd, path) pair to an absolute guest path.
fn at_path(t: &Thread, dirfd: i64, path: u64) -> Option<String> {
    let p = path_arg(path);
    if p.starts_with('/') {
        return Some(p);
    }
    let base = dir_path(t, dirfd)?;
    Some(fs::Vfs::normalize(&p, &base))
}

fn openat(t: &Thread, dirfd: i64, path: u64, flags: u32) -> i64 {
    let Some(path) = at_path(t, dirfd, path) else { return -EBADF };
    match t.proc.vfs.open(&path, "/", flags) {
        Ok(node) => {
            let f = fs::open_ref(node, flags);
            *f.path.lock().unwrap() = path.clone();
            t.proc.fds.lock().unwrap().alloc(f, flags & O_CLOEXEC != 0, 0)
        }
        Err(e) => -e,
    }
}

unsafe fn fstatat(t: &Thread, dirfd: i64, path: u64, buf: u64, flags: u32) -> i64 {
    let p = path_arg(path);
    if p.is_empty() && flags & AT_EMPTY_PATH != 0 {
        let Some(f) = t.proc.fds.lock().unwrap().get(dirfd) else { return -EBADF };
        return match fs::fstat(&f) {
            Ok(st) => {
                write_obj(buf, st);
                0
            }
            Err(e) => -e,
        };
    }
    let Some(full) = at_path(t, dirfd, path) else { return -EBADF };
    let r = if flags & AT_SYMLINK_NOFOLLOW != 0 { t.proc.vfs.lstat(&full, "/") } else { t.proc.vfs.stat(&full, "/") };
    match r {
        Ok(st) => {
            write_obj(buf, st);
            0
        }
        Err(e) => -e,
    }
}

unsafe fn statx(t: &Thread, dirfd: i64, path: u64, flags: u32, buf: u64) -> i64 {
    let mut st = Stat::default();
    let r = fstatat(t, dirfd, path, &mut st as *mut Stat as u64, flags);
    if r < 0 {
        return r;
    }
    let b = slice_mut(buf, 256);
    b.fill(0);
    let put32 = |b: &mut [u8], o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |b: &mut [u8], o: usize, v: u64| b[o..o + 8].copy_from_slice(&v.to_le_bytes());
    put32(b, 0, 0x7ff); // STATX_BASIC_STATS
    put32(b, 4, st.st_blksize as u32);
    put32(b, 16, st.st_nlink);
    put32(b, 20, st.st_uid);
    put32(b, 24, st.st_gid);
    b[28..30].copy_from_slice(&(st.st_mode as u16).to_le_bytes());
    put64(b, 32, st.st_ino);
    put64(b, 40, st.st_size as u64);
    put64(b, 48, st.st_blocks as u64);
    for (o, s, ns) in [(64, st.st_atime, st.st_atime_nsec), (80, st.st_mtime, st.st_mtime_nsec), (96, st.st_ctime, st.st_ctime_nsec), (112, st.st_mtime, st.st_mtime_nsec)] {
        put64(b, o, s as u64);
        put32(b, o + 8, ns as u32);
    }
    put32(b, 128, (st.st_rdev >> 8) as u32);
    put32(b, 132, (st.st_rdev & 0xff) as u32);
    put32(b, 136, (st.st_dev >> 8) as u32);
    put32(b, 140, (st.st_dev & 0xff) as u32);
    0
}

unsafe fn ioctl(f: &fs::Open, req: u64, arg: u64) -> i64 {
    const TCGETS: u64 = 0x5401;
    const TCSETS: u64 = 0x5402;
    const TCSETSW: u64 = 0x5403;
    const TIOCGWINSZ: u64 = 0x5413;
    const TIOCGPGRP: u64 = 0x540f;
    const FIONBIO: u64 = 0x5421;
    match req {
        TCGETS | TCSETS | TCSETSW | TIOCGPGRP | TIOCGWINSZ => {
            if !fs::is_tty(f) {
                return -ENOTTY;
            }
            match req {
                TCGETS => {
                    // struct termios (asm-generic, 60 bytes) with sane cooked-mode flags
                    let b = slice_mut(arg, 60);
                    b.fill(0);
                    b[0..4].copy_from_slice(&0x0500u32.to_le_bytes()); // iflag: ICRNL|IXON
                    b[4..8].copy_from_slice(&0x0005u32.to_le_bytes()); // oflag: OPOST|ONLCR
                    b[8..12].copy_from_slice(&0x00bfu32.to_le_bytes()); // cflag: B38400|CS8|CREAD
                    b[12..16].copy_from_slice(&0x8a3bu32.to_le_bytes()); // lflag: ISIG|ICANON|ECHO...
                }
                TIOCGWINSZ => {
                    let (rows, cols) = console_size();
                    write_obj(arg, [rows, cols, 0u16, 0u16]);
                }
                TIOCGPGRP => write_obj(arg, 1u32),
                _ => {}
            }
            0
        }
        FIONBIO => {
            // int *arg: nonzero = non-blocking
            let on = read_obj::<i32>(arg) != 0;
            let mut fl = f.flags.lock().unwrap();
            *fl = if on { *fl | O_NONBLOCK } else { *fl & !O_NONBLOCK };
            if let Node::Socket(s) = &mut *f.node.lock().unwrap() {
                s.nonblock = on;
                crate::net::set_nonblock(s, on);
            }
            0
        }
        0x541b => {
            // FIONREAD: bytes available to read
            let n: i64 = match &*f.node.lock().unwrap() {
                Node::Socket(s) => {
                    let mut v: u32 = 0;
                    if windows_sys::Win32::Networking::WinSock::ioctlsocket(s.s, windows_sys::Win32::Networking::WinSock::FIONREAD, &mut v) != 0 {
                        return -crate::net::last_errno();
                    }
                    v as i64
                }
                Node::Pipe(e) => crate::pipe::available(e) as i64,
                _ => return -ENOTTY,
            };
            write_obj(arg, n as i32);
            0
        }
        _ => -ENOTTY,
    }
}

fn console_size() -> (u16, u16) {
    use windows_sys::Win32::System::Console::*;
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(h, &mut info) != 0 {
            let cols = (info.srWindow.Right - info.srWindow.Left + 1) as u16;
            let rows = (info.srWindow.Bottom - info.srWindow.Top + 1) as u16;
            return (rows, cols);
        }
    }
    (24, 80)
}

fn fcntl(t: &Thread, fd: i64, cmd: u32, arg: u64) -> i64 {
    let mut fds = t.proc.fds.lock().unwrap();
    let Some(f) = fds.get(fd) else { return -EBADF };
    match cmd {
        0 | 1030 => fds.alloc(f, cmd == 1030, arg as usize), // F_DUPFD, F_DUPFD_CLOEXEC
        1 => fds.fds[fd as usize].as_ref().map(|e| e.cloexec as i64).unwrap_or(0),
        2 => {
            if let Some(e) = fds.fds[fd as usize].as_mut() {
                e.cloexec = arg & 1 != 0;
            }
            0
        }
        3 => *f.flags.lock().unwrap() as i64,
        4 => {
            let mut fl = f.flags.lock().unwrap();
            *fl = (*fl & !(O_APPEND | O_NONBLOCK)) | (arg as u32 & (O_APPEND | O_NONBLOCK));
            if let Node::Socket(s) = &mut *f.node.lock().unwrap() {
                s.nonblock = arg as u32 & O_NONBLOCK != 0;
                crate::net::set_nonblock(s, s.nonblock);
            }
            0
        }
        5..=7 | 36..=38 => 0, // record locks
        _ => -EINVAL,
    }
}

unsafe fn mmap(t: &Thread, a: [u64; 6]) -> i64 {
    let (addr, len, prot, flags, fd, off) = (a[0], a[1], a[2] as u32, a[3] as u32, a[4] as i32 as i64, a[5]);
    if len == 0 {
        return -EINVAL;
    }
    let mut fixed = flags & MAP_FIXED != 0;
    if flags & MAP_FIXED_NOREPLACE != 0 {
        if !mem::is_free(addr, len) {
            return -EEXIST;
        }
        fixed = true;
    }
    if fixed {
        wwd_cpu::invalidate_code(addr, len);
    }
    if flags & MAP_ANONYMOUS != 0 {
        return mem::map_anon(addr, len, prot, fixed);
    }
    // File mapping: private copy of the file contents.
    // TODO: MAP_SHARED write-back (needed for ashmem/gralloc buffers).
    let Some(f) = t.proc.fds.lock().unwrap().get(fd) else { return -EBADF };
    let r = mem::map_anon(addr, len, mem::PROT_READ | mem::PROT_WRITE, fixed);
    if r < 0 {
        return r;
    }
    {
        let path = f.path.lock().unwrap().clone();
        let mut fm = t.proc.file_maps.lock().unwrap();
        fm.retain(|(st, l, _, _)| *st + *l <= r as u64 || *st >= r as u64 + len);
        fm.push((r as u64, len, off, path.clone()));
        crate::resolve_breakpoints(&path, r as u64, len, off);
    }
    mem::prefault(r as u64, len);
    let buf = slice_mut(r as u64, len);
    let mut done = 0usize;
    while done < buf.len() {
        let n = fs::pread(&f, &mut buf[done..], off + done as u64);
        if n <= 0 {
            break;
        }
        done += n as usize;
    }
    if prot != mem::PROT_READ | mem::PROT_WRITE {
        mem::protect(r as u64, len, prot);
    }
    r
}

unsafe fn mremap(t: &Thread, a: [u64; 6]) -> i64 {
    let (old, old_sz, new_sz, flags) = (a[0], a[1], a[2], a[3]);
    let old_sz = mem::align_up(old_sz, mem::PAGE);
    let new_sz = mem::align_up(new_sz, mem::PAGE);
    if new_sz <= old_sz {
        if new_sz < old_sz {
            mem::unmap(old + new_sz, old_sz - new_sz);
        }
        return old as i64;
    }
    if flags & 1 == 0 {
        return -ENOMEM; // !MREMAP_MAYMOVE and we don't grow in place yet
    }
    let n = mem::map_anon(0, new_sz, mem::PROT_READ | mem::PROT_WRITE, false);
    if n < 0 {
        return n;
    }
    std::ptr::copy_nonoverlapping(old as *const u8, n as *mut u8, old_sz as usize);
    wwd_cpu::invalidate_code(old, old_sz);
    mem::unmap(old, old_sz);
    n
}

fn clock_now(clk: u64) -> Timespec {
    match clk {
        0 | 8 | 11 => {
            // REALTIME, REALTIME_ALARM, TAI
            let (s, ns) = crate::util::realtime();
            Timespec { sec: s, nsec: ns }
        }
        _ => {
            let ns = crate::util::monotonic_ns();
            Timespec { sec: (ns / 1_000_000_000) as i64, nsec: (ns % 1_000_000_000) as i64 }
        }
    }
}

fn sleep_ts(ts: Timespec) {
    if ts.sec > 0 || ts.nsec > 0 {
        std::thread::sleep(std::time::Duration::new(ts.sec.max(0) as u64, ts.nsec.clamp(0, 999_999_999) as u32));
    }
}

// ---------------------------------------------------------------------------
// futex on top of WaitOnAddress / WakeByAddress*
// ---------------------------------------------------------------------------

unsafe fn futex(a: [u64; 6]) -> i64 {
    use windows_sys::Win32::System::Threading::{WaitOnAddress, WakeByAddressAll, WakeByAddressSingle};
    let uaddr = a[0];
    let op = a[1] as u32;
    let cmd = op & 0x7f;
    let val = a[2] as u32;
    match cmd {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            let cur = std::ptr::read_volatile(uaddr as *const u32);
            if cur != val {
                return -EAGAIN;
            }
            let ms = if a[3] == 0 {
                u32::MAX // INFINITE
            } else {
                let ts: Timespec = read_obj(a[3]);
                let rel_ns: i128 = if cmd == FUTEX_WAIT {
                    ts.sec as i128 * 1_000_000_000 + ts.nsec as i128
                } else {
                    let now = clock_now(if op & FUTEX_CLOCK_REALTIME != 0 { 0 } else { 1 });
                    (ts.sec - now.sec) as i128 * 1_000_000_000 + (ts.nsec - now.nsec) as i128
                };
                if rel_ns <= 0 {
                    return -ETIMEDOUT;
                }
                ((rel_ns + 999_999) / 1_000_000).min(u32::MAX as i128 - 1) as u32
            };
            let expected = val;
            let r = WaitOnAddress(uaddr as *const _, &expected as *const u32 as *const _, 4, ms);
            if r == 0 {
                -ETIMEDOUT
            } else {
                0
            }
        }
        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            if val == 1 {
                WakeByAddressSingle(uaddr as *const _);
            } else if val > 0 {
                WakeByAddressAll(uaddr as *const _);
            }
            0
        }
        FUTEX_REQUEUE | FUTEX_CMP_REQUEUE => {
            if cmd == FUTEX_CMP_REQUEUE && std::ptr::read_volatile(uaddr as *const u32) != a[5] as u32 {
                return -EAGAIN;
            }
            // Waking everyone is a valid (if less efficient) requeue: waiters
            // re-check their condition and wait again.
            WakeByAddressAll(uaddr as *const _);
            WakeByAddressAll(a[4] as *const _);
            0
        }
        FUTEX_WAKE_OP => {
            let uaddr2 = a[4];
            let encoded = a[5] as u32;
            let wop = (encoded >> 28) & 7;
            let shift_arg = encoded & (8 << 28) != 0;
            let cmp = (encoded >> 24) & 15;
            let mut oparg = (encoded >> 12) & 0xfff;
            let cmparg = encoded & 0xfff;
            if shift_arg {
                oparg = 1 << (oparg & 31);
            }
            let at = std::sync::atomic::AtomicU32::from_ptr(uaddr2 as *mut u32);
            let old = match wop {
                0 => at.swap(oparg, Ordering::SeqCst),
                1 => at.fetch_add(oparg, Ordering::SeqCst),
                2 => at.fetch_or(oparg, Ordering::SeqCst),
                3 => at.fetch_and(!oparg, Ordering::SeqCst),
                _ => at.fetch_xor(oparg, Ordering::SeqCst),
            };
            WakeByAddressAll(uaddr as *const _);
            let hit = match cmp {
                0 => old == cmparg,
                1 => old != cmparg,
                2 => (old as i32) < cmparg as i32,
                3 => (old as i32) <= cmparg as i32,
                4 => (old as i32) > cmparg as i32,
                _ => (old as i32) >= cmparg as i32,
            };
            if hit {
                WakeByAddressAll(uaddr2 as *const _);
            }
            0
        }
        _ => -ENOSYS,
    }
}

// ---------------------------------------------------------------------------
// threads
// ---------------------------------------------------------------------------

unsafe fn thread_exit(t: &mut Thread) {
    if t.clear_child_tid != 0 {
        std::sync::atomic::AtomicU32::from_ptr(t.clear_child_tid as *mut u32).store(0, Ordering::SeqCst);
        windows_sys::Win32::System::Threading::WakeByAddressAll(t.clear_child_tid as *const _);
    }
}

unsafe fn clone(t: &mut Thread, a: [u64; 6]) -> i64 {
    let flags = a[0];
    let (stack, ptid, tls, ctid) = (a[1], a[2], a[3], a[4]);
    if flags & CLONE_THREAD == 0 || flags & CLONE_VM == 0 {
        // fork/vfork/posix_spawn: needs a second host process. Not yet.
        return -ENOSYS;
    }
    let tid = t.proc.next_tid.fetch_add(1, Ordering::Relaxed);
    let mut cpu = Box::new(wwd_cpu::Cpu::default());
    cpu.x = t.cpu.x;
    cpu.v = t.cpu.v;
    cpu.nzcv = t.cpu.nzcv;
    cpu.fpcr = t.cpu.fpcr;
    cpu.pc = t.cpu.pc; // already past the SVC
    cpu.tpidr = if flags & CLONE_SETTLS != 0 { tls } else { t.cpu.tpidr };
    cpu.x[0] = 0;
    if stack != 0 {
        cpu.set_sp(stack);
    }
    if flags & CLONE_PARENT_SETTID != 0 && ptid != 0 {
        write_obj(ptid, tid);
    }
    if flags & CLONE_CHILD_SETTID != 0 && ctid != 0 {
        write_obj(ctid, tid);
    }
    let mut child = Thread::new(t.proc.clone(), tid, cpu);
    child.sigmask = t.sigmask;
    if flags & CLONE_CHILD_CLEARTID != 0 {
        child.clear_child_tid = ctid;
    }
    let proc: Arc<crate::Process> = t.proc.clone();
    proc.live_threads.fetch_add(1, Ordering::AcqRel);
    let r = std::thread::Builder::new().name(format!("guest-{tid}")).stack_size(4 << 20).spawn(move || {
        let code = child.run();
        let p = child.proc.clone();
        if p.live_threads.fetch_sub(1, Ordering::AcqRel) == 1 {
            exit_process(&p, code);
        }
    });
    match r {
        Ok(_) => tid as i64,
        Err(_) => {
            proc.live_threads.fetch_sub(1, Ordering::AcqRel);
            -EAGAIN
        }
    }
}
