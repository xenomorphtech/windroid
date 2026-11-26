//! BSD sockets on Winsock: AF_INET / AF_INET6 stream and datagram sockets.
//!
//! Linux and Windows agree on the sockaddr_in/sockaddr_in6 layouts but not on
//! constants (AF_INET6, SOL_SOCKET, option numbers, errno), so those are
//! translated here.

use crate::abi::*;
use std::sync::Once;
use windows_sys::Win32::Networking::WinSock as ws;

pub const LINUX_AF_UNIX: u32 = 1;
pub const LINUX_AF_INET: u32 = 2;
pub const LINUX_AF_INET6: u32 = 10;
pub const SOCK_NONBLOCK: u32 = 0o4000;
pub const SOCK_CLOEXEC: u32 = 0o2000000;

pub const EINPROGRESS: i64 = 115;
pub const EAFNOSUPPORT: i64 = 97;

static INIT: Once = Once::new();

pub fn init() {
    INIT.call_once(|| unsafe {
        let mut data: ws::WSADATA = std::mem::zeroed();
        ws::WSAStartup(0x0202, &mut data);
    });
}

pub struct Socket {
    pub s: ws::SOCKET,
    pub family: u32,
    pub nonblock: bool,
}

impl Drop for Socket {
    fn drop(&mut self) {
        unsafe { ws::closesocket(self.s) };
    }
}

pub fn last_errno() -> i64 {
    let e = unsafe { ws::WSAGetLastError() };
    match e {
        ws::WSAEWOULDBLOCK => EAGAIN,
        ws::WSAEINPROGRESS | ws::WSAEALREADY => EINPROGRESS,
        ws::WSAECONNREFUSED => 111,
        ws::WSAETIMEDOUT => ETIMEDOUT,
        ws::WSAECONNRESET => 104,
        ws::WSAECONNABORTED => 103,
        ws::WSAENOTCONN => 107,
        ws::WSAEISCONN => 106,
        ws::WSAEADDRINUSE => 98,
        ws::WSAEADDRNOTAVAIL => 99,
        ws::WSAEHOSTUNREACH => 113,
        ws::WSAENETUNREACH => 101,
        ws::WSAENETDOWN => 100,
        ws::WSAEAFNOSUPPORT => EAFNOSUPPORT,
        ws::WSAEINVAL => EINVAL,
        ws::WSAEMSGSIZE => 90,
        ws::WSAENOBUFS => 105,
        ws::WSAEACCES => EACCES,
        ws::WSAESHUTDOWN => EPIPE,
        _ => {
            eprintln!("[wwd] net: unmapped winsock error {e}");
            EIO
        }
    }
}

fn win_family(f: u32) -> Option<i32> {
    match f {
        LINUX_AF_INET => Some(ws::AF_INET as i32),
        LINUX_AF_INET6 => Some(ws::AF_INET6 as i32),
        _ => None,
    }
}

pub fn socket(family: u32, ty: u32, proto: u32) -> Result<Socket, i64> {
    init();
    let wf = win_family(family).ok_or(EAFNOSUPPORT)?;
    let base = ty & 0xf; // SOCK_STREAM 1, SOCK_DGRAM 2, SOCK_RAW 3
    let s = unsafe { ws::socket(wf, base as i32, proto as i32) };
    if s == ws::INVALID_SOCKET {
        return Err(last_errno());
    }
    let sock = Socket { s, family, nonblock: ty & SOCK_NONBLOCK != 0 };
    if sock.nonblock {
        set_nonblock(&sock, true);
    }
    Ok(sock)
}

pub fn set_nonblock(s: &Socket, on: bool) {
    let mut v: u32 = on as u32;
    unsafe { ws::ioctlsocket(s.s, ws::FIONBIO, &mut v) };
}

/// Copy a guest sockaddr into a Windows-format buffer.
pub unsafe fn sockaddr_in(addr: u64, len: u32) -> Result<Vec<u8>, i64> {
    if addr == 0 || len < 2 {
        return Err(EINVAL);
    }
    let mut b = crate::mem::slice(addr, len.min(128) as u64).to_vec();
    let fam = u16::from_le_bytes([b[0], b[1]]) as u32;
    let wf = win_family(fam).ok_or(EAFNOSUPPORT)? as u16;
    b[0..2].copy_from_slice(&wf.to_le_bytes());
    Ok(b)
}

/// Write a Windows sockaddr back to the guest (with Linux family numbers).
pub unsafe fn sockaddr_out(win: &[u8], addr: u64, lenp: u64) {
    if addr == 0 || lenp == 0 {
        return;
    }
    let cap: u32 = crate::mem::read_obj(lenp);
    let mut b = win.to_vec();
    if b.len() >= 2 {
        let wf = u16::from_le_bytes([b[0], b[1]]);
        let lf: u16 = if wf == ws::AF_INET6 { LINUX_AF_INET6 as u16 } else { LINUX_AF_INET as u16 };
        b[0..2].copy_from_slice(&lf.to_le_bytes());
    }
    let n = (b.len() as u32).min(cap);
    std::ptr::copy_nonoverlapping(b.as_ptr(), crate::mem::untag(addr) as *mut u8, n as usize);
    crate::mem::write_obj(lenp, b.len() as u32);
}

pub fn ret(r: i32) -> i64 {
    if r == ws::SOCKET_ERROR {
        -last_errno()
    } else {
        r as i64
    }
}

/// Map (level, optname) from Linux to Windows. None = accept and ignore.
pub fn map_opt(level: i32, name: i32) -> Option<(i32, i32)> {
    match level {
        1 => {
            // SOL_SOCKET
            let w = match name {
                2 => ws::SO_REUSEADDR,
                4 => ws::SO_ERROR,
                6 => ws::SO_BROADCAST,
                7 => ws::SO_SNDBUF,
                8 => ws::SO_RCVBUF,
                9 => ws::SO_KEEPALIVE,
                13 => ws::SO_LINGER,
                20 => ws::SO_RCVTIMEO,
                21 => ws::SO_SNDTIMEO,
                3 => ws::SO_TYPE,
                _ => return None,
            };
            Some((ws::SOL_SOCKET, w))
        }
        6 => match name {
            1 => Some((ws::IPPROTO_TCP as i32, ws::TCP_NODELAY)),
            _ => None,
        },
        41 => match name {
            26 => Some((ws::IPPROTO_IPV6 as i32, ws::IPV6_V6ONLY)),
            _ => None,
        },
        _ => None,
    }
}

/// poll(2) readiness, Linux bits.
pub fn readiness(s: &Socket) -> i16 {
    let mut fd = ws::WSAPOLLFD { fd: s.s, events: (ws::POLLRDNORM | ws::POLLWRNORM) as i16, revents: 0 };
    let r = unsafe { ws::WSAPoll(&mut fd, 1, 0) };
    if r <= 0 {
        return 0;
    }
    let w = fd.revents as u16 as u32;
    let mut out = 0i16;
    if w & (ws::POLLRDNORM as u32 | ws::POLLRDBAND as u32) != 0 {
        out |= crate::pipe::POLLIN;
    }
    if w & ws::POLLWRNORM as u32 != 0 {
        out |= crate::pipe::POLLOUT;
    }
    if w & ws::POLLERR as u32 != 0 {
        out |= crate::pipe::POLLERR;
    }
    if w & ws::POLLHUP as u32 != 0 {
        out |= crate::pipe::POLLHUP | crate::pipe::POLLIN;
    }
    out
}

// ---- netd dnsproxyd emulation ----------------------------------------------
//
// bionic's getaddrinfo/gethostbyname send a text request to netd's
// /dev/socket/dnsproxyd and parse a binary reply. Our replacement
// libnetd_client opens /dev/socket/dnsproxyd, which wwd serves here using the
// host resolver.

fn be32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

struct HostAddr {
    family: u32, // Linux AF
    socktype: i32,
    protocol: i32,
    addr: Vec<u8>, // Linux-format sockaddr
}

fn host_lookup(host: Option<&str>, serv: Option<&str>, flags: i32, family: u32, socktype: i32, protocol: i32) -> Result<(Vec<HostAddr>, Option<String>), i32> {
    init();
    use std::ffi::CString;
    let h = host.map(|s| CString::new(s).unwrap());
    let sv = serv.map(|s| CString::new(s).unwrap());
    let mut hints: ws::ADDRINFOA = unsafe { std::mem::zeroed() };
    hints.ai_family = match family {
        LINUX_AF_INET => ws::AF_INET as i32,
        LINUX_AF_INET6 => ws::AF_INET6 as i32,
        _ => 0,
    };
    hints.ai_socktype = socktype;
    hints.ai_protocol = protocol;
    // AI_PASSIVE 1, AI_CANONNAME 2, AI_NUMERICHOST 4 match; ignore the rest
    hints.ai_flags = flags & 7;
    let mut res: *mut ws::ADDRINFOA = std::ptr::null_mut();
    let r = unsafe {
        ws::getaddrinfo(
            h.as_ref().map(|c| c.as_ptr() as *const u8).unwrap_or(std::ptr::null()),
            sv.as_ref().map(|c| c.as_ptr() as *const u8).unwrap_or(std::ptr::null()),
            &hints,
            &mut res,
        )
    };
    if r != 0 {
        // bionic EAI_* numbering: EAI_AGAIN 2, EAI_NONAME 8, EAI_SERVICE 9
        return Err(match r {
            ws::WSATRY_AGAIN => 2,
            ws::WSATYPE_NOT_FOUND => 9,
            _ => 8,
        });
    }
    let mut out = Vec::new();
    let mut canon = None;
    let mut p = res;
    while !p.is_null() {
        let ai = unsafe { &*p };
        if canon.is_none() && !ai.ai_canonname.is_null() {
            canon = Some(unsafe { std::ffi::CStr::from_ptr(ai.ai_canonname as *const i8) }.to_string_lossy().into_owned());
        }
        if ai.ai_family == ws::AF_INET as i32 || ai.ai_family == ws::AF_INET6 as i32 {
            let raw = unsafe { std::slice::from_raw_parts(ai.ai_addr as *const u8, ai.ai_addrlen) };
            let mut b = raw.to_vec();
            let lf = if ai.ai_family == ws::AF_INET6 as i32 { LINUX_AF_INET6 } else { LINUX_AF_INET };
            b[0..2].copy_from_slice(&(lf as u16).to_le_bytes());
            out.push(HostAddr { family: lf, socktype: ai.ai_socktype, protocol: ai.ai_protocol, addr: b });
        }
        p = ai.ai_next;
    }
    unsafe { ws::freeaddrinfo(res) };
    Ok((out, canon))
}

fn opt(s: &str) -> Option<&str> {
    if s == "^" { None } else { Some(s) }
}

fn dns_fail(out: &mut Vec<u8>, code: u32) {
    out.extend_from_slice(b"401\0");
    be32(out, code);
}

/// Answer one dnsproxyd request (without the trailing NUL).
pub fn dns_query(req: &str) -> Vec<u8> {
    let w: Vec<&str> = req.split(' ').collect();
    let mut out = Vec::new();
    let trace = std::env::var_os("WWD_TRACE_DNS").is_some();
    match w.first().copied() {
        Some("getaddrinfo") if w.len() >= 7 => {
            let flags: i32 = w[3].parse().unwrap_or(0);
            let r = host_lookup(opt(w[1]), opt(w[2]), flags, w[4].parse().unwrap_or(0), w[5].parse().unwrap_or(0), w[6].parse().unwrap_or(0));
            if trace {
                match &r {
                    Ok((v, _)) => eprintln!("[wwd] dns: {req} -> {} addresses", v.len()),
                    Err(e) => eprintln!("[wwd] dns: {req} -> EAI {e}"),
                }
            }
            match r {
                Ok((addrs, canon)) if !addrs.is_empty() => {
                    out.extend_from_slice(b"222\0");
                    for (i, a) in addrs.iter().enumerate() {
                        be32(&mut out, 1);
                        be32(&mut out, flags as u32);
                        be32(&mut out, a.family);
                        be32(&mut out, a.socktype as u32);
                        be32(&mut out, a.protocol as u32);
                        be32(&mut out, a.addr.len() as u32);
                        out.extend_from_slice(&a.addr);
                        match (&canon, i) {
                            (Some(c), 0) if flags & 2 != 0 => {
                                be32(&mut out, c.len() as u32 + 1);
                                out.extend_from_slice(c.as_bytes());
                                out.push(0);
                            }
                            _ => be32(&mut out, 0),
                        }
                    }
                    be32(&mut out, 0);
                }
                Ok(_) => dns_fail(&mut out, 8),
                Err(e) => dns_fail(&mut out, e as u32),
            }
        }
        Some("gethostbyname") if w.len() >= 4 => {
            // gethostbyname <netid> <name> <af>
            let af: u32 = w[3].parse().unwrap_or(LINUX_AF_INET);
            let r = host_lookup(Some(w[2]), None, 0, af, 1, 0);
            if trace {
                eprintln!("[wwd] dns: {req} -> {}", r.as_ref().map(|(v, _)| v.len() as i64).unwrap_or(-1));
            }
            match r {
                Ok((addrs, _)) if !addrs.is_empty() => {
                    out.extend_from_slice(b"222\0");
                    be32(&mut out, w[2].len() as u32 + 1);
                    out.extend_from_slice(w[2].as_bytes());
                    out.push(0);
                    be32(&mut out, 0); // no aliases
                    let len: u32 = if af == LINUX_AF_INET6 { 16 } else { 4 };
                    be32(&mut out, af);
                    be32(&mut out, len);
                    for a in &addrs {
                        let raw = if af == LINUX_AF_INET6 { &a.addr[8..24] } else { &a.addr[4..8] };
                        be32(&mut out, len);
                        out.extend_from_slice(raw);
                    }
                    be32(&mut out, 0);
                }
                _ => dns_fail(&mut out, 1),
            }
        }
        _ => {
            if trace {
                eprintln!("[wwd] dns: unsupported request {req:?}");
            }
            dns_fail(&mut out, 8)
        }
    }
    out
}
