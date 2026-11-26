//! In-process pipes and poll readiness.

use crate::abi::*;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PIPE_BUF_MAX: usize = 64 * 1024;

pub struct Pipe {
    st: Mutex<State>,
    cv: Condvar,
}

struct State {
    data: VecDeque<u8>,
    readers: usize,
    writers: usize,
}

/// One end of a pipe; dropping it closes that end.
pub struct End {
    pub pipe: Arc<Pipe>,
    pub write: bool,
}

impl Drop for End {
    fn drop(&mut self) {
        let mut s = self.pipe.st.lock().unwrap();
        if self.write {
            s.writers -= 1;
        } else {
            s.readers -= 1;
        }
        drop(s);
        self.pipe.cv.notify_all();
        notify_poll();
    }
}

pub fn new_pair() -> (End, End) {
    let p = Arc::new(Pipe { st: Mutex::new(State { data: VecDeque::new(), readers: 1, writers: 1 }), cv: Condvar::new() });
    (End { pipe: p.clone(), write: false }, End { pipe: p, write: true })
}

pub fn read(p: &Pipe, buf: &mut [u8], nonblock: bool) -> i64 {
    let mut s = p.st.lock().unwrap();
    loop {
        if !s.data.is_empty() {
            let n = buf.len().min(s.data.len());
            for (i, b) in s.data.drain(..n).enumerate() {
                buf[i] = b;
            }
            drop(s);
            p.cv.notify_all();
            notify_poll();
            return n as i64;
        }
        if s.writers == 0 {
            return 0;
        }
        if nonblock || buf.is_empty() {
            return -EAGAIN;
        }
        s = p.cv.wait(s).unwrap();
    }
}

pub fn write(p: &Pipe, buf: &[u8], nonblock: bool) -> i64 {
    let mut s = p.st.lock().unwrap();
    let mut done = 0;
    while done < buf.len() {
        if s.readers == 0 {
            return if done > 0 { done as i64 } else { -EPIPE };
        }
        let room = PIPE_BUF_MAX.saturating_sub(s.data.len());
        if room == 0 {
            if nonblock {
                return if done > 0 { done as i64 } else { -EAGAIN };
            }
            s = p.cv.wait(s).unwrap();
            continue;
        }
        let k = room.min(buf.len() - done);
        s.data.extend(&buf[done..done + k]);
        done += k;
        p.cv.notify_all();
        notify_poll();
    }
    done as i64
}

pub const POLLIN: i16 = 1;
pub const POLLOUT: i16 = 4;
pub const POLLERR: i16 = 8;
pub const POLLHUP: i16 = 16;
pub const POLLNVAL: i16 = 32;

pub fn readiness(e: &End) -> i16 {
    let s = e.pipe.st.lock().unwrap();
    if e.write {
        if s.readers == 0 {
            POLLERR
        } else if s.data.len() < PIPE_BUF_MAX {
            POLLOUT
        } else {
            0
        }
    } else {
        let mut r = 0;
        if !s.data.is_empty() {
            r |= POLLIN;
        }
        if s.writers == 0 {
            r |= POLLHUP;
        }
        r
    }
}

// A process-wide "something changed" signal for poll waiters.
static POLL: (Mutex<u64>, Condvar) = (Mutex::new(0), Condvar::new());

pub fn notify_poll() {
    let mut g = POLL.0.lock().unwrap();
    *g = g.wrapping_add(1);
    POLL.1.notify_all();
}

/// Wait until `check()` returns Some or the timeout passes (None = forever).
pub fn wait_until<T>(timeout: Option<Duration>, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        let gen = *POLL.0.lock().unwrap();
        if let Some(v) = check() {
            return Some(v);
        }
        let now = Instant::now();
        let wait = match deadline {
            Some(d) if now >= d => return None,
            Some(d) => (d - now).min(Duration::from_millis(5)),
            None => Duration::from_millis(5),
        };
        let g = POLL.0.lock().unwrap();
        if *g == gen {
            let _ = POLL.1.wait_timeout(g, wait).unwrap();
        }
    }
}

/// eventfd(2): a 64-bit counter.
pub struct EventFd {
    v: Mutex<u64>,
    cv: Condvar,
    sema: bool,
}

pub fn eventfd(init: u64, sema: bool) -> Arc<EventFd> {
    Arc::new(EventFd { v: Mutex::new(init), cv: Condvar::new(), sema })
}

pub fn eventfd_read(e: &EventFd, buf: &mut [u8], nonblock: bool) -> i64 {
    if buf.len() < 8 {
        return -EINVAL;
    }
    let mut v = e.v.lock().unwrap();
    while *v == 0 {
        if nonblock {
            return -EAGAIN;
        }
        v = e.cv.wait(v).unwrap();
    }
    let out = if e.sema { 1 } else { *v };
    *v -= out;
    buf[..8].copy_from_slice(&out.to_le_bytes());
    drop(v);
    e.cv.notify_all();
    notify_poll();
    8
}

pub fn eventfd_write(e: &EventFd, buf: &[u8]) -> i64 {
    if buf.len() < 8 {
        return -EINVAL;
    }
    let add = u64::from_le_bytes(buf[..8].try_into().unwrap());
    if add == u64::MAX {
        return -EINVAL;
    }
    let mut v = e.v.lock().unwrap();
    *v = v.saturating_add(add).min(u64::MAX - 1);
    drop(v);
    e.cv.notify_all();
    notify_poll();
    8
}

pub fn eventfd_readiness(e: &EventFd) -> i16 {
    if *e.v.lock().unwrap() > 0 { POLLIN | POLLOUT } else { POLLOUT }
}

/// Bytes buffered in a pipe (FIONREAD).
pub fn available(e: &End) -> usize {
    if e.write { 0 } else { e.pipe.st.lock().unwrap().data.len() }
}
