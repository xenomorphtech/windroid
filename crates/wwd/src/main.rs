//! wwd — run AArch64 Linux binaries on Windows (milestone 1 of an Android runtime).
//!
//! usage: wwd [options] <program> [args...]
//!   --root DIR     guest root filesystem (default: current directory)
//!   --interp       use the reference interpreter instead of the JIT
//!   --strace       log every syscall to stderr
//!   --stats        print JIT statistics on exit
//!   --env K=V      add an environment variable (repeatable)
//!
//! <program> is a host path; it is made visible to the guest relative to --root
//! when it lives inside it, otherwise its directory becomes the root.

mod scan;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use wwd_cpu::Mode;
use wwd_linux::Config;

fn usage() -> ! {
    eprintln!("usage: wwd [--root DIR] [--interp] [--strace] [--stats] [--env K=V]... <program> [args...]");
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut root: Option<PathBuf> = None;
    let mut mode = Mode::Jit;
    let mut strace = false;
    let mut stats = false;
    let mut profile: Option<PathBuf> = None;
    let mut env = vec!["PATH=/usr/local/bin:/usr/bin:/bin".to_string(), "HOME=/".to_string(), "LANG=C.UTF-8".to_string(), "TERM=xterm-256color".to_string()];
    while let Some(a) = args.peek() {
        if !a.starts_with("--") {
            break;
        }
        let a = args.next().unwrap();
        match a.as_str() {
            "--root" => root = Some(PathBuf::from(args.next().unwrap_or_else(|| usage()))),
            "--interp" => mode = Mode::Interp,
            "--strace" => strace = true,
            "--stats" => stats = true,
            "--profile" => profile = Some(PathBuf::from(args.next().unwrap_or_else(|| usage()))),
            "--disasm" => {
                let f = args.next().unwrap_or_else(|| usage());
                let o = args.next().unwrap_or_else(|| usage());
                let n = args.next().unwrap_or_else(|| "32".into());
                scan::disasm_file(&f, &o, &n);
                return;
            }
            "--scan" => {
                scan::run(&args.next().unwrap_or_else(|| usage()));
                return;
            }
            "--env" => env.push(args.next().unwrap_or_else(|| usage())),
            "--" => break,
            _ => usage(),
        }
    }
    let Some(prog) = args.next() else { usage() };
    let rest: Vec<String> = args.collect();

    let prog_host = std::fs::canonicalize(&prog).unwrap_or_else(|e| {
        eprintln!("wwd: {prog}: {e}");
        std::process::exit(127);
    });
    let root = match root {
        Some(r) => std::fs::canonicalize(r).expect("bad --root"),
        None => {
            let cwd = std::fs::canonicalize(".").unwrap();
            if prog_host.starts_with(&cwd) {
                cwd
            } else {
                prog_host.parent().unwrap().to_path_buf()
            }
        }
    };
    let guest_path = guest_path_of(&root, &prog_host).unwrap_or_else(|| {
        eprintln!("wwd: {} is outside the guest root {}", prog_host.display(), root.display());
        std::process::exit(127);
    });

    unsafe {
        // UTF-8 console so guest output renders correctly
        windows_sys::Win32::System::Console::SetConsoleOutputCP(65001);
    }
    wwd_linux::install_fault_handler();

    let cfg = Config { mode, strace, stats, root, env, profile };
    let mut argv = vec![guest_path.clone()];
    argv.extend(rest);

    // Run the guest on a thread with a generous stack (deep interpreter frames).
    let h = std::thread::Builder::new()
        .name("guest-main".into())
        .stack_size(16 << 20)
        .spawn(move || {
            let mut t = match wwd_linux::spawn(cfg, &guest_path, &argv) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("wwd: {e}");
                    std::process::exit(126);
                }
            };
            let start = std::time::Instant::now();
            let code = t.run();
            if t.proc.cfg.stats {
                report(&t, start.elapsed());
            }
            // main thread called exit(); the process lives on while others run
            if t.proc.live_threads.fetch_sub(1, Ordering::AcqRel) == 1 {
                std::process::exit(code);
            }
            loop {
                std::thread::park();
            }
        })
        .unwrap();
    let _ = h.join();
}

fn report(t: &wwd_linux::Thread, el: std::time::Duration) {
    eprintln!("[wwd] main thread ran {:.3}s", el.as_secs_f64());
    if let Some(s) = t.engine.stats() {
        eprintln!("[wwd] jit: {s:?}");
    } else {
        eprintln!("[wwd] interpreter steps: {}", t.engine.interp_steps);
    }
}

fn guest_path_of(root: &Path, host: &Path) -> Option<String> {
    let rel = host.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    Some(format!("/{}", parts.join("/")))
}
