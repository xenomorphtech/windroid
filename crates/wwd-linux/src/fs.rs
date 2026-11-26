//! File descriptors and the guest's view of the filesystem.
//!
//! The guest sees a Linux-style tree rooted at a host directory (`--root`),
//! plus a few synthetic nodes (/dev/null, /dev/urandom, /proc/self/exe...).

use crate::abi::*;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};

pub enum Node {
    Console { handle: HANDLE, is_tty: bool },
    Host { file: File, path: PathBuf },
    Dir { path: PathBuf, entries: Option<Vec<(String, u8, u64)>>, pos: usize, links: Vec<String> },
    Null,
    Zero,
    Random,
    /// Read-only synthetic content (e.g. /proc/cpuinfo).
    Mem { data: Vec<u8>, pos: usize },
    Pipe(crate::pipe::End),
    Socket(crate::net::Socket),
    EventFd(std::sync::Arc<crate::pipe::EventFd>),
    /// netd dnsproxyd connection: request bytes in, reply bytes out
    Dns { req: Vec<u8>, resp: Vec<u8>, pos: usize },
}

unsafe impl Send for Node {}

pub struct Open {
    pub node: Mutex<Node>,
    pub flags: Mutex<u32>,
    /// guest path it was opened by (for /proc/self/fd/N)
    pub path: Mutex<String>,
}

pub type OpenRef = Arc<Open>;

#[derive(Clone)]
pub struct FdEntry {
    pub file: OpenRef,
    pub cloexec: bool,
}

pub struct FdTable {
    pub fds: Vec<Option<FdEntry>>,
}

fn console(which: u32) -> Node {
    let handle = unsafe { GetStdHandle(which) };
    let mut mode = 0;
    let is_tty = unsafe { GetConsoleMode(handle, &mut mode) } != 0;
    Node::Console { handle, is_tty }
}

pub fn open_ref(node: Node, flags: u32) -> OpenRef {
    Arc::new(Open { node: Mutex::new(node), flags: Mutex::new(flags), path: Mutex::new(String::new()) })
}

impl FdTable {
    pub fn new_stdio() -> FdTable {
        let mut t = FdTable { fds: Vec::new() };
        t.fds.push(Some(FdEntry { file: open_ref(console(STD_INPUT_HANDLE), 0), cloexec: false }));
        t.fds.push(Some(FdEntry { file: open_ref(console(STD_OUTPUT_HANDLE), O_WRONLY), cloexec: false }));
        t.fds.push(Some(FdEntry { file: open_ref(console(STD_ERROR_HANDLE), O_WRONLY), cloexec: false }));
        t
    }
    pub fn get(&self, fd: i64) -> Option<OpenRef> {
        if fd < 0 {
            return None;
        }
        self.fds.get(fd as usize).and_then(|e| e.as_ref()).map(|e| e.file.clone())
    }
    pub fn alloc(&mut self, file: OpenRef, cloexec: bool, min: usize) -> i64 {
        let mut i = min;
        loop {
            if i >= self.fds.len() {
                self.fds.resize(i + 1, None);
            }
            if self.fds[i].is_none() {
                self.fds[i] = Some(FdEntry { file, cloexec });
                return i as i64;
            }
            i += 1;
        }
    }
    pub fn close(&mut self, fd: i64) -> i64 {
        if fd < 0 || fd as usize >= self.fds.len() || self.fds[fd as usize].is_none() {
            return -EBADF;
        }
        self.fds[fd as usize] = None;
        0
    }
    pub fn set_at(&mut self, fd: usize, file: OpenRef, cloexec: bool) {
        if fd >= self.fds.len() {
            self.fds.resize(fd + 1, None);
        }
        self.fds[fd] = Some(FdEntry { file, cloexec });
    }
}

// ---------------------------------------------------------------------------
// I/O on nodes
// ---------------------------------------------------------------------------

fn fill_random(buf: &mut [u8]) {
    crate::util::random_bytes(buf);
}

pub fn read(f: &Open, buf: &mut [u8]) -> i64 {
    let nonblock = *f.flags.lock().unwrap() & O_NONBLOCK != 0;
    let mut n = f.node.lock().unwrap();
    if let Node::Pipe(e) = &*n {
        let p = e.pipe.clone();
        drop(n);
        return crate::pipe::read(&p, buf, nonblock);
    }
    if let Node::EventFd(e) = &*n {
        let e = e.clone();
        drop(n);
        return crate::pipe::eventfd_read(&e, buf, nonblock);
    }
    if let Node::Socket(s) = &*n {
        let h = s.s;
        drop(n);
        let r = unsafe { windows_sys::Win32::Networking::WinSock::recv(h, buf.as_mut_ptr(), buf.len().min(i32::MAX as usize) as i32, 0) };
        return crate::net::ret(r);
    }
    match &mut *n {
        Node::Console { handle, .. } => {
            let mut got = 0u32;
            let ok = unsafe { ReadFile(*handle, buf.as_mut_ptr(), buf.len().min(u32::MAX as usize) as u32, &mut got, std::ptr::null_mut()) };
            if ok == 0 {
                0 // EOF / broken pipe
            } else {
                got as i64
            }
        }
        Node::Host { file, .. } => match file.read(buf) {
            Ok(n) => n as i64,
            Err(e) => -io_errno(&e),
        },
        Node::Dir { .. } => -EISDIR,
        Node::Dns { resp, pos, .. } => {
            let k = buf.len().min(resp.len() - *pos);
            buf[..k].copy_from_slice(&resp[*pos..*pos + k]);
            *pos += k;
            k as i64
        }
        Node::Null => 0,
        Node::Pipe(_) | Node::Socket(_) | Node::EventFd(_) => unreachable!(),
        Node::Zero => {
            buf.fill(0);
            buf.len() as i64
        }
        Node::Random => {
            fill_random(buf);
            buf.len() as i64
        }
        Node::Mem { data, pos } => {
            let n = buf.len().min(data.len().saturating_sub(*pos));
            buf[..n].copy_from_slice(&data[*pos..*pos + n]);
            *pos += n;
            n as i64
        }
    }
}

pub fn write(f: &Open, buf: &[u8]) -> i64 {
    let flags = *f.flags.lock().unwrap();
    let append = flags & O_APPEND != 0;
    let mut n = f.node.lock().unwrap();
    if let Node::Pipe(e) = &*n {
        let p = e.pipe.clone();
        drop(n);
        return crate::pipe::write(&p, buf, flags & O_NONBLOCK != 0);
    }
    if let Node::EventFd(e) = &*n {
        let e = e.clone();
        drop(n);
        return crate::pipe::eventfd_write(&e, buf);
    }
    if let Node::Socket(s) = &*n {
        let h = s.s;
        drop(n);
        if std::env::var_os("WWD_TRACE_NET").is_some() {
            eprintln!("[wwd] net send {} bytes: {}", buf.len(), String::from_utf8_lossy(&buf[..buf.len().min(2000)]));
        }
        let r = unsafe { windows_sys::Win32::Networking::WinSock::send(h, buf.as_ptr(), buf.len().min(i32::MAX as usize) as i32, 0) };
        return crate::net::ret(r);
    }
    match &mut *n {
        Node::Console { handle, .. } => {
            let mut done = 0u32;
            let ok = unsafe { WriteFile(*handle, buf.as_ptr(), buf.len().min(u32::MAX as usize) as u32, &mut done, std::ptr::null_mut()) };
            if ok == 0 {
                -EPIPE
            } else {
                done as i64
            }
        }
        Node::Host { file, .. } => {
            if append {
                let _ = file.seek(SeekFrom::End(0));
            }
            match file.write(buf) {
                Ok(n) => n as i64,
                Err(e) => -io_errno(&e),
            }
        }
        Node::Dir { .. } => -EISDIR,
        Node::Null | Node::Zero | Node::Random => buf.len() as i64,
        Node::Dns { req, resp, pos } => {
            req.extend_from_slice(buf);
            while let Some(z) = req.iter().position(|&b| b == 0) {
                let q: Vec<u8> = req.drain(..=z).collect();
                let text = String::from_utf8_lossy(&q[..z]).into_owned();
                resp.drain(..*pos);
                *pos = 0;
                resp.extend(crate::net::dns_query(&text));
            }
            buf.len() as i64
        }
        Node::Mem { .. } => -EBADF,
        Node::Pipe(_) | Node::Socket(_) | Node::EventFd(_) => unreachable!(),
    }
}

/// poll(2) readiness for an open file.
pub fn poll_events(f: &Open) -> i16 {
    use crate::pipe::*;
    match &*f.node.lock().unwrap() {
        Node::Pipe(e) => readiness(e),
        Node::Socket(s) => crate::net::readiness(s),
        Node::EventFd(e) => crate::pipe::eventfd_readiness(e),
        Node::Console { .. } => POLLOUT,
        _ => POLLIN | POLLOUT,
    }
}

pub fn pread(f: &Open, buf: &mut [u8], off: u64) -> i64 {
    let mut n = f.node.lock().unwrap();
    match &mut *n {
        Node::Host { file, .. } => {
            use std::os::windows::fs::FileExt;
            match file.seek_read(buf, off) {
                Ok(n) => n as i64,
                Err(e) if e.raw_os_error() == Some(38) => 0, // ERROR_HANDLE_EOF
                Err(e) => -io_errno(&e),
            }
        }
        Node::Mem { data, .. } => {
            let off = off as usize;
            let n = buf.len().min(data.len().saturating_sub(off));
            buf[..n].copy_from_slice(&data[off..off + n]);
            n as i64
        }
        _ => -ESPIPE,
    }
}

pub fn pwrite(f: &Open, buf: &[u8], off: u64) -> i64 {
    let mut n = f.node.lock().unwrap();
    match &mut *n {
        Node::Host { file, .. } => {
            use std::os::windows::fs::FileExt;
            match file.seek_write(buf, off) {
                Ok(n) => n as i64,
                Err(e) => -io_errno(&e),
            }
        }
        _ => -ESPIPE,
    }
}

pub fn lseek(f: &Open, off: i64, whence: u32) -> i64 {
    let mut n = f.node.lock().unwrap();
    match &mut *n {
        Node::Host { file, .. } => {
            let pos = match whence {
                0 => SeekFrom::Start(off as u64),
                1 => SeekFrom::Current(off),
                2 => SeekFrom::End(off),
                _ => return -EINVAL,
            };
            match file.seek(pos) {
                Ok(p) => p as i64,
                Err(e) => -io_errno(&e),
            }
        }
        Node::Mem { data, pos } => {
            let np = match whence {
                0 => off,
                1 => *pos as i64 + off,
                2 => data.len() as i64 + off,
                _ => return -EINVAL,
            };
            if np < 0 {
                return -EINVAL;
            }
            *pos = np as usize;
            np
        }
        Node::Dir { pos, entries, .. } => {
            if whence == 0 && off == 0 {
                *pos = 0;
                *entries = None;
                0
            } else {
                -EINVAL
            }
        }
        Node::Null | Node::Zero | Node::Random => 0,
        Node::Console { .. } | Node::Pipe(_) | Node::Socket(_) | Node::EventFd(_) | Node::Dns { .. } => -ESPIPE,
    }
}

pub fn is_tty(f: &Open) -> bool {
    matches!(&*f.node.lock().unwrap(), Node::Console { is_tty: true, .. })
}

fn ino_of(p: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.to_string_lossy().to_lowercase().hash(&mut h);
    h.finish() | 1
}

fn systime(t: std::io::Result<std::time::SystemTime>) -> (i64, u64) {
    match t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()) {
        Some(d) => (d.as_secs() as i64, d.subsec_nanos() as u64),
        None => (0, 0),
    }
}

pub fn stat_meta(path: &Path, md: &std::fs::Metadata) -> Stat {
    let mut st = Stat::default();
    st.st_dev = 0x801;
    st.st_ino = ino_of(path);
    st.st_mode = if md.is_dir() {
        S_IFDIR | 0o755
    } else if md.file_type().is_symlink() {
        S_IFLNK | 0o777
    } else {
        S_IFREG | if md.permissions().readonly() { 0o555 } else { 0o755 }
    };
    st.st_nlink = 1;
    st.st_uid = 0;
    st.st_gid = 0;
    st.st_size = md.len() as i64;
    st.st_blksize = 4096;
    st.st_blocks = (md.len() as i64 + 511) / 512;
    let (a, an) = systime(md.accessed());
    let (m, mn) = systime(md.modified());
    st.st_atime = a;
    st.st_atime_nsec = an;
    st.st_mtime = m;
    st.st_mtime_nsec = mn;
    st.st_ctime = m;
    st.st_ctime_nsec = mn;
    st
}

pub fn char_dev_stat(rdev: u64) -> Stat {
    Stat { st_dev: 5, st_ino: rdev, st_mode: S_IFCHR | 0o666, st_nlink: 1, st_rdev: rdev, st_blksize: 4096, ..Default::default() }
}

pub fn fstat(f: &Open) -> Result<Stat, i64> {
    let n = f.node.lock().unwrap();
    match &*n {
        Node::Console { .. } => Ok(char_dev_stat(0x8800)),
        Node::Host { file, path } => file.metadata().map(|m| stat_meta(path, &m)).map_err(|e| io_errno(&e)),
        Node::Dir { path, .. } => std::fs::metadata(path).map(|m| stat_meta(path, &m)).map_err(|e| io_errno(&e)),
        Node::Null => Ok(char_dev_stat(0x103)),
        Node::Zero => Ok(char_dev_stat(0x105)),
        Node::Random => Ok(char_dev_stat(0x109)),
        Node::EventFd(_) | Node::Dns { .. } => Ok(Stat { st_mode: 0o600, st_nlink: 1, st_blksize: 4096, ..Default::default() }),
        Node::Socket(_) => Ok(Stat { st_mode: 0o140000 | 0o777, st_nlink: 1, st_blksize: 4096, ..Default::default() }),
        Node::Pipe(_) => Ok(Stat { st_mode: S_IFIFO | 0o600, st_nlink: 1, st_blksize: 4096, ..Default::default() }),
        Node::Mem { data, .. } => Ok(Stat { st_mode: S_IFREG | 0o444, st_size: data.len() as i64, st_nlink: 1, st_blksize: 4096, ..Default::default() }),
    }
}

pub fn getdents64(f: &Open, buf: &mut [u8]) -> i64 {
    let mut n = f.node.lock().unwrap();
    let Node::Dir { path, entries, pos, links } = &mut *n else { return -ENOTDIR };
    if entries.is_none() {
        let mut v = vec![(".".to_string(), 4u8, 1u64), ("..".to_string(), 4u8, 2u64)];
        if let Ok(rd) = std::fs::read_dir(&*path) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let t = match e.file_type() {
                    Ok(t) if t.is_dir() => 4,
                    Ok(t) if t.is_symlink() => 10,
                    _ => 8,
                };
                v.push((name, t, ino_of(&e.path())));
            }
        }
        for l in links.iter() {
            if !v.iter().any(|(n, _, _)| n == l) {
                v.push((l.clone(), 10, ino_of(&path.join(l))));
            }
        }
        *entries = Some(v);
    }
    let ents = entries.as_ref().unwrap();
    let mut off = 0usize;
    while *pos < ents.len() {
        let (name, t, ino) = &ents[*pos];
        let reclen = (19 + name.len() + 1 + 7) & !7;
        if off + reclen > buf.len() {
            break;
        }
        let r = &mut buf[off..off + reclen];
        r.fill(0);
        r[0..8].copy_from_slice(&ino.to_le_bytes());
        r[8..16].copy_from_slice(&((*pos + 1) as i64).to_le_bytes());
        r[16..18].copy_from_slice(&(reclen as u16).to_le_bytes());
        r[18] = *t;
        r[19..19 + name.len()].copy_from_slice(name.as_bytes());
        off += reclen;
        *pos += 1;
    }
    if off == 0 && *pos < ents.len() {
        return -EINVAL;
    }
    off as i64
}

pub fn io_errno(e: &std::io::Error) -> i64 {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => ENOENT,
        PermissionDenied => EACCES,
        AlreadyExists => EEXIST,
        InvalidInput => EINVAL,
        UnexpectedEof => EIO,
        _ => match e.raw_os_error() {
            Some(267) => ENOTDIR,   // ERROR_DIRECTORY
            Some(145) => ENOTEMPTY, // ERROR_DIR_NOT_EMPTY
            Some(3) => ENOENT,      // ERROR_PATH_NOT_FOUND
            _ => EIO,
        },
    }
}

// ---------------------------------------------------------------------------
// path resolution
// ---------------------------------------------------------------------------

pub enum Resolved {
    Host(PathBuf),
    Special(&'static str),
}

pub struct Vfs {
    pub root: PathBuf,
    /// Guest-visible path of the main executable (for /proc/self/exe).
    pub exe: String,
    /// NUL-separated argv for /proc/self/cmdline
    pub cmdline: Mutex<Vec<u8>>,
    /// Symlinks recorded in `<root>/.wwd_symlinks` (guest path -> target),
    /// because unprivileged Windows can't create real ones.
    pub links: std::collections::HashMap<String, String>,
    /// directory -> names of symlinks inside it (for getdents)
    pub dir_links: std::collections::HashMap<String, Vec<String>>,
}

impl Vfs {
    pub fn new(root: PathBuf, exe: String) -> Vfs {
        let mut links = std::collections::HashMap::new();
        let mut dir_links: std::collections::HashMap<String, Vec<String>> = Default::default();
        if let Ok(s) = std::fs::read_to_string(root.join(".wwd_symlinks")) {
            for line in s.lines() {
                if let Some((from, to)) = line.split_once('\t') {
                    let from = Self::normalize(from, "/");
                    if let Some((dir, name)) = from.rsplit_once('/') {
                        let dir = if dir.is_empty() { "/".to_string() } else { dir.to_string() };
                        dir_links.entry(dir).or_default().push(name.to_string());
                    }
                    links.insert(from, to.to_string());
                }
            }
        }
        Vfs { root, exe, links, dir_links, cmdline: Mutex::new(Vec::new()) }
    }

    /// Canonical guest path with recorded symlinks followed per component.
    pub fn canon(&self, path: &str, cwd: &str, follow_last: bool) -> Result<String, i64> {
        let mut out: Vec<String> = Vec::new();
        let start = if path.starts_with('/') { path.to_string() } else { format!("{cwd}/{path}") };
        let mut rest: Vec<String> = start.split('/').rev().map(String::from).collect();
        let mut hops = 0;
        while let Some(c) = rest.pop() {
            match c.as_str() {
                "" | "." => continue,
                ".." => {
                    out.pop();
                    continue;
                }
                _ => {}
            }
            out.push(c);
            let last = rest.iter().all(|r| r.is_empty() || r == ".");
            if last && !follow_last {
                break;
            }
            if self.links.is_empty() {
                continue;
            }
            let cur = format!("/{}", out.join("/"));
            if let Some(t) = self.links.get(&cur) {
                hops += 1;
                if hops > 40 {
                    return Err(ELOOP);
                }
                out.pop();
                if t.starts_with('/') {
                    out.clear();
                }
                rest.extend(t.split('/').rev().map(String::from));
            }
        }
        Ok(format!("/{}", out.join("/")))
    }

    pub fn readlink(&self, path: &str, cwd: &str) -> Option<String> {
        let p = self.canon(path, cwd, false).ok()?;
        self.links.get(&p).cloned()
    }

    pub fn host_of(&self, guest_canon: &str) -> PathBuf {
        let mut h = self.root.clone();
        for c in guest_canon.split('/').filter(|c| !c.is_empty()) {
            h.push(c);
        }
        h
    }

    /// Normalise a guest path against `cwd` (both Linux style), never escaping /.
    pub fn normalize(path: &str, cwd: &str) -> String {
        let full = if path.starts_with('/') { path.to_string() } else { format!("{}/{}", cwd.trim_end_matches('/'), path) };
        let mut parts: Vec<&str> = Vec::new();
        for c in full.split('/') {
            match c {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                c => parts.push(c),
            }
        }
        format!("/{}", parts.join("/"))
    }

    pub fn resolve(&self, path: &str, cwd: &str) -> Resolved {
        self.resolve_opt(path, cwd, true)
    }

    pub fn resolve_opt(&self, path: &str, cwd: &str, follow: bool) -> Resolved {
        let p = match self.canon(path, cwd, follow) {
            Ok(p) => p,
            Err(_) => Self::normalize(path, cwd),
        };
        match p.as_str() {
            "/dev/null" => return Resolved::Special("null"),
            "/dev/zero" => return Resolved::Special("zero"),
            "/dev/urandom" | "/dev/random" => return Resolved::Special("random"),
            "/dev/tty" | "/dev/stdout" | "/proc/self/fd/1" => return Resolved::Special("stdout"),
            "/dev/stderr" | "/proc/self/fd/2" => return Resolved::Special("stderr"),
            "/dev/stdin" | "/proc/self/fd/0" => return Resolved::Special("stdin"),
            "/proc/self/exe" => return Resolved::Special("exe"),
            "/proc/cpuinfo" => return Resolved::Special("cpuinfo"),
            "/proc/self/maps" => return Resolved::Special("maps"),
            "/proc/meminfo" => return Resolved::Special("meminfo"),
            "/dev/__properties__" => return Resolved::Special("properties"),
            "/dev/socket/dnsproxyd" => return Resolved::Special("dnsproxyd"),
            "/proc/self/cmdline" => return Resolved::Special("cmdline"),
            "/proc/self/status" => return Resolved::Special("status"),
            _ => {}
        }
        Resolved::Host(self.host_of(&p))
    }

    /// lstat: report recorded symlinks as links, else like stat.
    pub fn lstat(&self, path: &str, cwd: &str) -> Result<Stat, i64> {
        if let Ok(p) = self.canon(path, cwd, false) {
            if let Some(t) = self.links.get(&p) {
                return Ok(Stat { st_mode: S_IFLNK | 0o777, st_nlink: 1, st_size: t.len() as i64, st_ino: ino_of(Path::new(&p)), st_blksize: 4096, ..Default::default() });
            }
        }
        self.stat(path, cwd)
    }

    pub fn special_node(&self, name: &str) -> Option<Node> {
        Some(match name {
            "null" => Node::Null,
            "zero" => Node::Zero,
            "random" => Node::Random,
            "stdout" => console(STD_OUTPUT_HANDLE),
            "stderr" => console(STD_ERROR_HANDLE),
            "stdin" => console(STD_INPUT_HANDLE),
            "cpuinfo" => Node::Mem { data: cpuinfo().into_bytes(), pos: 0 },
            "meminfo" => Node::Mem { data: b"MemTotal:       8388608 kB\nMemFree:        4194304 kB\nMemAvailable:   4194304 kB\n".to_vec(), pos: 0 },
            "maps" => Node::Mem { data: Vec::new(), pos: 0 },
            "cmdline" => Node::Mem { data: self.cmdline.lock().unwrap().clone(), pos: 0 },
            "status" => Node::Mem { data: b"Name:	wwd
State:	R (running)
TracerPid:	0
Uid:	10100	10100	10100	10100
Gid:	10100	10100	10100	10100
Threads:	1
VmRSS:	100000 kB
".to_vec(), pos: 0 },
            "dnsproxyd" => Node::Dns { req: Vec::new(), resp: Vec::new(), pos: 0 },
            "properties" => Node::Mem { data: crate::props::build(&crate::props::default_props(&self.root)), pos: 0 },
            _ => return None,
        })
    }

    pub fn open(&self, path: &str, cwd: &str, flags: u32) -> Result<Node, i64> {
        match self.resolve(path, cwd) {
            Resolved::Special("exe") => {
                let Resolved::Host(h) = self.resolve(&self.exe, "/") else { return Err(ENOENT) };
                let file = File::open(&h).map_err(|e| io_errno(&e))?;
                Ok(Node::Host { file, path: h })
            }
            Resolved::Special(s) => self.special_node(s).ok_or(ENOENT),
            Resolved::Host(h) => {
                if h.is_dir() {
                    if flags & O_ACCMODE != 0 {
                        return Err(EISDIR);
                    }
                    let g = self.canon(path, cwd, true).unwrap_or_default();
                    let links = self.dir_links.get(&g).cloned().unwrap_or_default();
                    return Ok(Node::Dir { path: h, entries: None, pos: 0, links });
                }
                if flags & O_DIRECTORY != 0 {
                    return Err(if h.exists() { ENOTDIR } else { ENOENT });
                }
                let mut o = OpenOptions::new();
                match flags & O_ACCMODE {
                    0 => o.read(true),
                    O_WRONLY => o.write(true),
                    _ => o.read(true).write(true),
                };
                if flags & O_CREAT != 0 {
                    if flags & O_EXCL != 0 {
                        o.create_new(true);
                    } else {
                        o.create(true);
                    }
                    if flags & O_ACCMODE == 0 {
                        o.write(true);
                    }
                }
                if flags & O_TRUNC != 0 {
                    o.truncate(true);
                }
                let file = o.open(&h).map_err(|e| io_errno(&e))?;
                Ok(Node::Host { file, path: h })
            }
        }
    }

    pub fn stat(&self, path: &str, cwd: &str) -> Result<Stat, i64> {
        match self.resolve(path, cwd) {
            Resolved::Special("exe") => self.stat(&self.exe.clone(), "/"),
            Resolved::Special(s) => {
                let n = self.special_node(s).ok_or(ENOENT)?;
                let o = Open { node: Mutex::new(n), flags: Mutex::new(0), path: Mutex::new(String::new()) };
                fstat(&o)
            }
            Resolved::Host(h) => {
                let md = std::fs::metadata(&h).map_err(|e| io_errno(&e))?;
                Ok(stat_meta(&h, &md))
            }
        }
    }
}

fn cpuinfo() -> String {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut s = String::new();
    for i in 0..n {
        s += &format!(
            "processor\t: {i}\nBogoMIPS\t: 2000.00\nFeatures\t: fp asimd evtstrm crc32 atomics cpuid\nCPU implementer\t: 0x41\nCPU architecture: 8\nCPU variant\t: 0x0\nCPU part\t: 0xd0c\nCPU revision\t: 0\n\n"
        );
    }
    s
}
