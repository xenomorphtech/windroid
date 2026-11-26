# wwd — an Android runtime for Windows (no WSL, no VM)

Goal: run unmodified Android apps (dex + arm64 native libs) as native Windows
processes, the way Waydroid runs them on Linux, but without a Linux kernel.

```
 Android app (.apk: dex + arm64 .so)
        │
 ┌──────┴────────────────────────────────────────────────┐
 │ AOSP userspace built for x86_64 (ART, framework,       │  runs natively
 │ SurfaceFlinger, bionic)                                │
 │   • bionic syscall stubs → wwd-linux (in-process)      │
 │   • NativeBridge → wwd-cpu translates arm64 .so files  │
 └──────┬───────────────────────┬────────────────────────┘
        │                       │
 [1] wwd-cpu                [2] wwd-linux
     AArch64 → x86-64 JIT       Linux syscalls → Win32: memory, futex,
                                threads, files, signals, binder, ashmem
 [3] graphics: gralloc/HWC → Win32 window; GLES via ANGLE; Vulkan passthrough
 [4] input, audio (WASAPI), network, storage
```

The key design choice, shared with Waydroid, is an **x86_64 Android image**:
only the app's ARM native code is translated, not the whole OS.

## Crates

| crate | role |
|---|---|
| `wwd-cpu` | A64 decoder (`decode.rs`), reference interpreter (`interp.rs`, `simd.rs`), block JIT (`jit/`), `Engine` dispatcher |
| `wwd-linux` | ELF loader, initial stack/auxv, guest memory on `VirtualAlloc`, syscalls, fd table and VFS, threads |
| `wwd` | CLI: `wwd [--interp] [--strace] [--root DIR] prog args…`, `wwd --scan elf` |

### CPU (`wwd-cpu`)
* Guest memory is **identity-mapped**: a guest address is a host pointer.
* The **interpreter is the source of truth**. The JIT emits native x86-64 for
  hot instructions (ALU, loads/stores, branches, flags) and calls the
  interpreter for everything else, so the JIT never lacks coverage.
* Guest registers stay in the `Cpu` struct (rbx points at it). That makes the
  guest state exact at any fault, which signal delivery will need.
* Conditions: one `bt` against a 16-bit truth table (`COND_TABLE`) per condition code.
* `wwd --scan` statically decodes a binary and lists encodings we can't execute.
* Tests run under both engines and the outputs must match byte for byte.

### Linux personality (`wwd-linux`)
* `mmap`/`munmap`/`mprotect` are built on `VirtualAlloc` reservations
  (64 KiB granules vs 4 KiB Linux pages are handled in `mem.rs`).
* `futex` maps onto `WaitOnAddress`/`WakeByAddress*`, which fits closely.
* `clone(CLONE_THREAD)` creates a host thread with its own `Engine`.
* Changes to guest code (`mprotect +X`, `munmap`, `IC IVAU`) bump a code
  generation counter, and every thread flushes its translations.

## Status: milestone 1 ✅
Static aarch64-linux-musl binaries run correctly: stdio, malloc, libm,
setjmp, file I/O, mmap, pthreads, mutexes, atomics (`./test.sh`).

## Roadmap
1. **JIT performance**: block chaining plus an inline lookup cache, a register
   cache for hot guest registers, lazy flags, native FP/SIMD templates, and a
   shared code cache across threads.
2. **Dynamic linking**: run glibc/musl `ld.so` and dynamically linked
   binaries (PT_INTERP is already loaded); `MAP_SHARED` file mappings.
3. **Signals**: deliver SIGSEGV/SIGBUS/SIGILL/SIGALRM with a real `ucontext`
   (ART relies on SIGSEGV for null checks and stack overflow detection).
4. **Processes**: fork/exec/vfork via new host processes; pipes, poll/epoll,
   eventfd, sockets (Winsock), and the full `/proc` that Android expects.
5. **Bionic + Android userspace**: boot `linker64` and a minimal x86_64 AOSP
   (servicemanager, ART `dalvikvm` hello world). Implement binder and ashmem
   in-process.
6. **NativeBridge**: a `libnb` that loads arm64 `.so` into x86_64 ART with
   JNI trampolines in both directions (what libhoudini/libndk_translation do).
7. **Graphics**: gralloc on shared memory, HWComposer → Win32 window,
   EGL/GLES → ANGLE (D3D11) and Vulkan → host Vulkan with format/feature
   translation.
8. **Input, audio, app install UX**: an `.apk` launcher and Start-menu shortcuts.
