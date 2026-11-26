//! Linux/arm64 ABI constants and structure layouts.

pub const EPERM: i64 = 1;
pub const ENOENT: i64 = 2;
pub const ESRCH: i64 = 3;
pub const EINTR: i64 = 4;
pub const EIO: i64 = 5;
pub const EBADF: i64 = 9;
pub const ECHILD: i64 = 10;
pub const EAGAIN: i64 = 11;
pub const ENOMEM: i64 = 12;
pub const EACCES: i64 = 13;
pub const EFAULT: i64 = 14;
pub const EEXIST: i64 = 17;
pub const ENOTDIR: i64 = 20;
pub const EISDIR: i64 = 21;
pub const EINVAL: i64 = 22;
pub const EMFILE: i64 = 24;
pub const ENOTTY: i64 = 25;
pub const ESPIPE: i64 = 29;
pub const EPIPE: i64 = 32;
pub const ERANGE: i64 = 34;
pub const ENOSYS: i64 = 38;
pub const ENOTSOCK: i64 = 88;
pub const ENOTEMPTY: i64 = 39;
pub const ELOOP: i64 = 40;
pub const ENODATA: i64 = 61;
pub const EOPNOTSUPP: i64 = 95;
pub const ETIMEDOUT: i64 = 110;

// open flags (arm64 == asm-generic)
pub const O_ACCMODE: u32 = 3;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECTORY: u32 = 0o40000;
pub const O_CLOEXEC: u32 = 0o2000000;

pub const AT_FDCWD: i64 = -100;
pub const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
pub const AT_EMPTY_PATH: u32 = 0x1000;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFLNK: u32 = 0o120000;

pub const MAP_SHARED: u32 = 0x01;
pub const MAP_PRIVATE: u32 = 0x02;
pub const MAP_FIXED: u32 = 0x10;
pub const MAP_ANONYMOUS: u32 = 0x20;
pub const MAP_FIXED_NOREPLACE: u32 = 0x100000;

pub const CLONE_VM: u64 = 0x100;
pub const CLONE_FS: u64 = 0x200;
pub const CLONE_FILES: u64 = 0x400;
pub const CLONE_SIGHAND: u64 = 0x800;
pub const CLONE_VFORK: u64 = 0x4000;
pub const CLONE_THREAD: u64 = 0x10000;
pub const CLONE_SETTLS: u64 = 0x80000;
pub const CLONE_PARENT_SETTID: u64 = 0x100000;
pub const CLONE_CHILD_CLEARTID: u64 = 0x200000;
pub const CLONE_CHILD_SETTID: u64 = 0x1000000;

pub const FUTEX_WAIT: u32 = 0;
pub const FUTEX_WAKE: u32 = 1;
pub const FUTEX_REQUEUE: u32 = 3;
pub const FUTEX_CMP_REQUEUE: u32 = 4;
pub const FUTEX_WAKE_OP: u32 = 5;
pub const FUTEX_WAIT_BITSET: u32 = 9;
pub const FUTEX_WAKE_BITSET: u32 = 10;
pub const FUTEX_CLOCK_REALTIME: u32 = 256;

// auxv
pub const AT_NULL: u64 = 0;
pub const AT_PHDR: u64 = 3;
pub const AT_PHENT: u64 = 4;
pub const AT_PHNUM: u64 = 5;
pub const AT_PAGESZ: u64 = 6;
pub const AT_BASE: u64 = 7;
pub const AT_FLAGS: u64 = 8;
pub const AT_ENTRY: u64 = 9;
pub const AT_UID: u64 = 11;
pub const AT_EUID: u64 = 12;
pub const AT_GID: u64 = 13;
pub const AT_EGID: u64 = 14;
pub const AT_PLATFORM: u64 = 15;
pub const AT_HWCAP: u64 = 16;
pub const AT_CLKTCK: u64 = 17;
pub const AT_SECURE: u64 = 23;
pub const AT_RANDOM: u64 = 25;
pub const AT_HWCAP2: u64 = 26;
pub const AT_EXECFN: u64 = 31;

// arm64 HWCAP bits we can honour
pub const HWCAP_FP: u64 = 1 << 0;
pub const HWCAP_ASIMD: u64 = 1 << 1;
pub const HWCAP_EVTSTRM: u64 = 1 << 2;
pub const HWCAP_CRC32: u64 = 1 << 7;
pub const HWCAP_ATOMICS: u64 = 1 << 8;
pub const HWCAP_CPUID: u64 = 1 << 11;

/// asm-generic `struct stat` (128 bytes on arm64).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_mode: u32,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub __pad1: u64,
    pub st_size: i64,
    pub st_blksize: i32,
    pub __pad2: i32,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: u64,
    pub st_mtime: i64,
    pub st_mtime_nsec: u64,
    pub st_ctime: i64,
    pub st_ctime_nsec: u64,
    pub __unused: [u32; 2],
}
const _: () = assert!(std::mem::size_of::<Stat>() == 128);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Iovec {
    pub base: u64,
    pub len: u64,
}

pub mod nr {
    pub const GETCWD: u64 = 17;
    pub const EVENTFD2: u64 = 19;
    pub const EPOLL_CREATE1: u64 = 20;
    pub const DUP: u64 = 23;
    pub const DUP3: u64 = 24;
    pub const FCNTL: u64 = 25;
    pub const IOCTL: u64 = 29;
    pub const MKDIRAT: u64 = 34;
    pub const UNLINKAT: u64 = 35;
    pub const RENAMEAT: u64 = 38;
    pub const STATFS: u64 = 43;
    pub const FSTATFS: u64 = 44;
    pub const FTRUNCATE: u64 = 46;
    pub const FCHDIR: u64 = 50;
    pub const FACCESSAT: u64 = 48;
    pub const CHDIR: u64 = 49;
    pub const FCHMOD: u64 = 52;
    pub const FCHMODAT: u64 = 53;
    pub const FCHOWNAT: u64 = 54;
    pub const OPENAT: u64 = 56;
    pub const CLOSE: u64 = 57;
    pub const PIPE2: u64 = 59;
    pub const GETDENTS64: u64 = 61;
    pub const LSEEK: u64 = 62;
    pub const READ: u64 = 63;
    pub const WRITE: u64 = 64;
    pub const READV: u64 = 65;
    pub const WRITEV: u64 = 66;
    pub const PREAD64: u64 = 67;
    pub const PWRITE64: u64 = 68;
    pub const PPOLL: u64 = 73;
    pub const READLINKAT: u64 = 78;
    pub const NEWFSTATAT: u64 = 79;
    pub const FSTAT: u64 = 80;
    pub const FSYNC: u64 = 82;
    pub const FDATASYNC: u64 = 83;
    pub const EXIT: u64 = 93;
    pub const EXIT_GROUP: u64 = 94;
    pub const SET_TID_ADDRESS: u64 = 96;
    pub const FUTEX: u64 = 98;
    pub const SET_ROBUST_LIST: u64 = 99;
    pub const NANOSLEEP: u64 = 101;
    pub const CLOCK_GETTIME: u64 = 113;
    pub const CLOCK_GETRES: u64 = 114;
    pub const CLOCK_NANOSLEEP: u64 = 115;
    pub const SCHED_SETAFFINITY: u64 = 122;
    pub const SCHED_GETAFFINITY: u64 = 123;
    pub const SCHED_YIELD: u64 = 124;
    pub const KILL: u64 = 129;
    pub const TKILL: u64 = 130;
    pub const TGKILL: u64 = 131;
    pub const SIGALTSTACK: u64 = 132;
    pub const RT_SIGSUSPEND: u64 = 133;
    pub const RT_SIGACTION: u64 = 134;
    pub const RT_SIGPROCMASK: u64 = 135;
    pub const SETPRIORITY: u64 = 140;
    pub const SETREGID: u64 = 143;
    pub const SETGID: u64 = 144;
    pub const SETREUID: u64 = 145;
    pub const SETUID: u64 = 146;
    pub const SETRESUID: u64 = 147;
    pub const GETRESUID: u64 = 148;
    pub const SETRESGID: u64 = 149;
    pub const GETRESGID: u64 = 150;
    pub const SETPGID: u64 = 154;
    pub const GETPGID: u64 = 155;
    pub const GETSID: u64 = 156;
    pub const SETSID: u64 = 157;
    pub const GETGROUPS: u64 = 158;
    pub const GETPRIORITY: u64 = 141;
    pub const UNAME: u64 = 160;
    pub const GETRLIMIT: u64 = 163;
    pub const SETRLIMIT: u64 = 164;
    pub const GETRUSAGE: u64 = 165;
    pub const UMASK: u64 = 166;
    pub const PRCTL: u64 = 167;
    pub const GETTIMEOFDAY: u64 = 169;
    pub const GETPID: u64 = 172;
    pub const GETPPID: u64 = 173;
    pub const GETUID: u64 = 174;
    pub const GETEUID: u64 = 175;
    pub const GETGID: u64 = 176;
    pub const GETEGID: u64 = 177;
    pub const GETTID: u64 = 178;
    pub const SYSINFO: u64 = 179;
    pub const SOCKET: u64 = 198;
    pub const BRK: u64 = 214;
    pub const MUNMAP: u64 = 215;
    pub const MREMAP: u64 = 216;
    pub const CLONE: u64 = 220;
    pub const EXECVE: u64 = 221;
    pub const MMAP: u64 = 222;
    pub const MPROTECT: u64 = 226;
    pub const MSYNC: u64 = 227;
    pub const MADVISE: u64 = 233;
    pub const WAIT4: u64 = 260;
    pub const PRLIMIT64: u64 = 261;
    pub const GETRANDOM: u64 = 278;
    pub const MEMBARRIER: u64 = 283;
    pub const STATX: u64 = 291;
    pub const RSEQ: u64 = 293;
    pub const CLONE3: u64 = 435;
    pub const FACCESSAT2: u64 = 439;
}

pub fn syscall_name(n: u64) -> &'static str {
    use nr::*;
    match n {
        GETCWD => "getcwd",
        DUP => "dup",
        DUP3 => "dup3",
        FCNTL => "fcntl",
        IOCTL => "ioctl",
        MKDIRAT => "mkdirat",
        UNLINKAT => "unlinkat",
        FACCESSAT => "faccessat",
        CHDIR => "chdir",
        OPENAT => "openat",
        CLOSE => "close",
        PIPE2 => "pipe2",
        GETDENTS64 => "getdents64",
        LSEEK => "lseek",
        READ => "read",
        WRITE => "write",
        READV => "readv",
        WRITEV => "writev",
        PREAD64 => "pread64",
        PWRITE64 => "pwrite64",
        PPOLL => "ppoll",
        READLINKAT => "readlinkat",
        NEWFSTATAT => "newfstatat",
        FSTAT => "fstat",
        EXIT => "exit",
        EXIT_GROUP => "exit_group",
        SET_TID_ADDRESS => "set_tid_address",
        FUTEX => "futex",
        SET_ROBUST_LIST => "set_robust_list",
        NANOSLEEP => "nanosleep",
        CLOCK_GETTIME => "clock_gettime",
        CLOCK_GETRES => "clock_getres",
        CLOCK_NANOSLEEP => "clock_nanosleep",
        SCHED_GETAFFINITY => "sched_getaffinity",
        SCHED_YIELD => "sched_yield",
        KILL => "kill",
        TKILL => "tkill",
        TGKILL => "tgkill",
        SIGALTSTACK => "sigaltstack",
        RT_SIGACTION => "rt_sigaction",
        RT_SIGPROCMASK => "rt_sigprocmask",
        139 => "rt_sigreturn",
        UNAME => "uname",
        GETRLIMIT => "getrlimit",
        PRCTL => "prctl",
        GETTIMEOFDAY => "gettimeofday",
        GETPID => "getpid",
        GETTID => "gettid",
        BRK => "brk",
        MUNMAP => "munmap",
        MREMAP => "mremap",
        CLONE => "clone",
        MMAP => "mmap",
        MPROTECT => "mprotect",
        MADVISE => "madvise",
        PRLIMIT64 => "prlimit64",
        GETRANDOM => "getrandom",
        STATX => "statx",
        RSEQ => "rseq",
        _ => "?",
    }
}
