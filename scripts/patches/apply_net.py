import os
os.chdir(r"C:\work\wwd")
here = os.path.join("scripts", "patches")


def patch(p, pairs, append=None):
    s = open(p, encoding="utf-8").read()
    for a, b in pairs:
        if a not in s:
            raise SystemExit(f"{p}: anchor not found: {a[:90]}")
        s = s.replace(a, b, 1)
    if append:
        s += append
    open(p, "w", encoding="utf-8").write(s)


patch("crates/wwd-linux/src/lib.rs", [("pub mod mem;", "pub mod mem;\npub mod net;")])
patch("crates/wwd-linux/Cargo.toml", [('"Win32_UI_Input_KeyboardAndMouse"]', '"Win32_UI_Input_KeyboardAndMouse", "Win32_Networking_WinSock"]')])

patch("crates/wwd-linux/src/fs.rs", [
    ("""    Pipe(crate::pipe::End),
}""", """    Pipe(crate::pipe::End),
    Socket(crate::net::Socket),
}"""),
    # read
    ("""    if let Node::Pipe(e) = &*n {
        let p = e.pipe.clone();
        drop(n);
        return crate::pipe::read(&p, buf, nonblock);
    }""", """    if let Node::Pipe(e) = &*n {
        let p = e.pipe.clone();
        drop(n);
        return crate::pipe::read(&p, buf, nonblock);
    }
    if let Node::Socket(s) = &*n {
        let h = s.s;
        drop(n);
        let r = unsafe { windows_sys::Win32::Networking::WinSock::recv(h, buf.as_mut_ptr(), buf.len().min(i32::MAX as usize) as i32, 0) };
        return crate::net::ret(r);
    }"""),
    # write
    ("""    if let Node::Pipe(e) = &*n {
        let p = e.pipe.clone();
        drop(n);
        return crate::pipe::write(&p, buf, flags & O_NONBLOCK != 0);
    }""", """    if let Node::Pipe(e) = &*n {
        let p = e.pipe.clone();
        drop(n);
        return crate::pipe::write(&p, buf, flags & O_NONBLOCK != 0);
    }
    if let Node::Socket(s) = &*n {
        let h = s.s;
        drop(n);
        let r = unsafe { windows_sys::Win32::Networking::WinSock::send(h, buf.as_ptr(), buf.len().min(i32::MAX as usize) as i32, 0) };
        return crate::net::ret(r);
    }"""),
    ("""        Node::Pipe(e) => readiness(e),""", """        Node::Pipe(e) => readiness(e),
        Node::Socket(s) => crate::net::readiness(s),"""),
    ("""        Node::Pipe(_) => Ok(Stat {""", """        Node::Socket(_) => Ok(Stat { st_mode: 0o140000 | 0o777, st_nlink: 1, st_blksize: 4096, ..Default::default() }),
        Node::Pipe(_) => Ok(Stat {"""),
    ("""        Node::Mem { .. } => -EBADF,
        Node::Pipe(_) => unreachable!(),""", """        Node::Mem { .. } => -EBADF,
        Node::Pipe(_) | Node::Socket(_) => unreachable!(),"""),
    ("""        Node::Null => 0,
        Node::Pipe(_) => unreachable!(),""", """        Node::Null => 0,
        Node::Pipe(_) | Node::Socket(_) => unreachable!(),"""),
    ("""        Node::Console { .. } | Node::Pipe(_) => -ESPIPE,""", """        Node::Console { .. } | Node::Pipe(_) | Node::Socket(_) => -ESPIPE,"""),
])

calls = open(os.path.join(here, "net_syscalls.rs.txt"), encoding="utf-8").read()
patch("crates/wwd-linux/src/sys.rs", [
    ("""            EVENTFD2 | EPOLL_CREATE1 | SOCKET => ok(-ENOSYS),""", calls + """            EVENTFD2 | EPOLL_CREATE1 => ok(-ENOSYS),"""),
    ("""            204 | 205 => ok(-ENOTSOCK),
""", ""),
    ("""fn with_fd(t: &Thread, fd: u64, f: impl FnOnce(&fs::Open) -> i64) -> Outcome {""", """fn with_sock(t: &Thread, fd: u64, f: impl FnOnce(&crate::net::Socket) -> i64) -> Outcome {
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

fn with_fd(t: &Thread, fd: u64, f: impl FnOnce(&fs::Open) -> i64) -> Outcome {"""),
    # fcntl O_NONBLOCK must reach the socket
    ("""        4 => {
            let mut fl = f.flags.lock().unwrap();
            *fl = (*fl & !(O_APPEND | O_NONBLOCK)) | (arg as u32 & (O_APPEND | O_NONBLOCK));
            0
        }""", """        4 => {
            let mut fl = f.flags.lock().unwrap();
            *fl = (*fl & !(O_APPEND | O_NONBLOCK)) | (arg as u32 & (O_APPEND | O_NONBLOCK));
            if let Node::Socket(s) = &mut *f.node.lock().unwrap() {
                s.nonblock = arg as u32 & O_NONBLOCK != 0;
                crate::net::set_nonblock(s, s.nonblock);
            }
            0
        }"""),
])
# poll loop: sockets don't notify our condvar, so wake more often
patch("crates/wwd-linux/src/pipe.rs", [("Some(d) => (d - now).min(Duration::from_millis(50)),\n            None => Duration::from_millis(50),",
                                        "Some(d) => (d - now).min(Duration::from_millis(5)),\n            None => Duration::from_millis(5),")])
print("patched")
