# windroid (wwd)

Run Android ARM64 apps as native Windows processes, with no WSL and no VM.

wwd ships these parts:

- **CPU translator:** an AArch64 interpreter and an AArch64 → x86-64 JIT.
- **Linux syscall layer:** emulates the Linux kernel ABI on top of Win32 and Winsock.
- **Vulkan thunks:** pass guest Vulkan calls to the host driver, with ASTC texture emulation.
- **Android C libraries (the droid layer):** a small set of libraries built with Zig, covering a JNI/JVM stand-in, libandroid, EGL/GLES, OpenSL ES/AAudio and the netd client.

The app runs against the real bionic libc from the Android 14 (API 34) system image.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design.

**Status:** Overgeared, an Unreal Engine 5 game, runs through login, server select and character select into the in-game world against a local server. It runs at about 6 fps, there is no audio output yet, and the scene renders with some visual errors.

## Requirements

- Windows 10/11 x64 with a Vulkan 1.3 GPU driver.
- [Rust](https://rustup.rs), stable, using the `x86_64-pc-windows-gnu` toolchain.
- Git Bash. The build and run scripts are bash.
- Python 3. The setup scripts use only the standard library.
- [Zig](https://ziglang.org/download/) 0.17.1, unpacked to `tools/zig-x86_64-windows-0.17.1/`.

Everything under `tools/`, `rootfs*/` and `games/` is downloaded or generated, so it isn't in git.

Run all commands below from the repo root in Git Bash, with `export MSYS_NO_PATHCONV=1` set.

## 1. Build the runtime

```sh
cargo build --release          # → target/release/wwd.exe
./test.sh                      # optional: AArch64 test suite on interpreter and JIT
```

`test.sh` builds static musl test programs, runs them under both the interpreter and the JIT, and requires identical output.

```sh
target/release/wwd.exe [--root DIR] [--interp] [--strace] [--stats] [--env K=V]... <program> [args...]
target/release/wwd.exe --scan <elf>     # list A64 encodings the decoder can't handle
```

## 2. Build the Android guest root

wwd needs files from the Android 14 arm64 emulator system image:

- bionic and the dynamic linker;
- `/system/lib64`;
- fonts and tzdata.

```sh
# system image (≈1.5 GB) and bionic headers
mkdir -p tools/sysimg && cd tools/sysimg
curl -LO https://dl.google.com/android/repository/sys-img/android/arm64-v8a-34_r04.zip
python3 -c "import zipfile; zipfile.ZipFile('arm64-v8a-34_r04.zip').extractall()"
cd ../..
mkdir -p tools/bionic-headers && cd tools/bionic-headers
for d in libc/include libc/kernel/uapi libc/kernel/android/uapi; do
  n=$(echo $d | tr / _); mkdir -p $n
  curl -sSfL "https://android.googlesource.com/platform/bionic/+archive/refs/heads/android14-release/$d.tar.gz" | tar -xz -C $n
done
cd ../..

# extract into rootfs-android
I=tools/sysimg/arm64-v8a/system.img; R=rootfs-android
A="python3 -I scripts/androidimg.py"
$A apex    $I system /system/apex/com.android.runtime.apex $R /apex/com.android.runtime
$A apex    $I system /system/apex/com.android.tzdata.apex  $R /apex/com.android.tzdata
$A extract $I system /system/lib64       $R /system/lib64
$A extract $I system /system/build.prop  $R /system/build.prop
$A extract $I system /system/fonts       $R /system/fonts
$A extract $I system /system/etc/fonts.xml $R /system/etc/fonts.xml

# build the droid layer into rootfs-android/wwd/{lib64,bin}
droid/build.sh
```

`androidimg.py` reads GPT, the dynamic `super` partition, ext4 and APEX images directly. It records symlinks in `rootfs-android/.wwd_symlinks`, which the wwd VFS reads.

## 3. Install an app

```sh
python3 -I scripts/install_apk.py rootfs-android base.apk [split.apk ...]
```

The script sets up the app the way PackageManager would:

- it creates `/data/app/<pkg>/` with the APKs and the extracted `lib/arm64/*.so`;
- it creates the data directories under `/data/user/0/<pkg>` and `/storage/emulated/0/Android/{data,obb}/<pkg>`;
- it writes the manifest metadata.

A file at `/data/app/<pkg>/asset_overrides/<name>` replaces the APK asset of that name.

## 4. Run

The guest entry point is `/wwd/bin/wwd-app`. It loads the app's native activity and drives it.

```sh
target/release/wwd.exe --root rootfs-android \
  --env LD_LIBRARY_PATH=/wwd/lib64:/data/app/<pkg>/lib/arm64:/system/lib64 \
  rootfs-android/wwd/bin/wwd-app <pkg> <libname>
```

The app's window opens on the desktop. Mouse clicks become touch events. `scripts/click.ps1 -X x -Y y` sends a click to the window, and `scripts/shot.ps1 out.png` screenshots it.

### Overgeared (Unreal Engine 5)

1. Fetch the local-server APK set (stub `1.0.5-183-local-server`) into `games/overgeared-local/`, then install it. Its `libUnreal.so` is already patched; see `native-patch.json`.

   ```sh
   G=games/overgeared-local
   python3 -I scripts/install_apk.py rootfs-android $G/com.nexon.overgeared.apk \
     $G/config.arm64_v8a.apk $G/config.en.apk $G/config.mdpi.apk $G/obbassets.apk
   mkdir -p rootfs-android/data/app/com.nexon.overgeared/asset_overrides
   printf '%s' '-project="../../../Temppal/Temppal.uproject" -AllowCPUDevices -vulkan' \
     > rootfs-android/data/app/com.nexon.overgeared/asset_overrides/UECommandLine.txt
   ```

2. Copy the game's downloaded resource cache, about 15 GB, to `rootfs-android/storage/emulated/0/Android/data/com.nexon.overgeared/files/PUB/`. The patcher stand-in reports it as up to date.

3. Start the local servers and make them reachable on 127.0.0.1 ports 18080 (bootstrap), 10000 (Satisfy) and 10002 (Grid). For example, use an SSH tunnel to the server host:

   ```sh
   ssh -N -L 18080:127.0.0.1:18081 -L 10000:127.0.0.1:10001 -L 10002:127.0.0.1:10002 <user>@<server-host>
   ```

4. Run the game, then tap the title screen and press 게임 시작 (Game Start):

   ```sh
   ./run-og.sh            # optional: ./run-og.sh <seconds> [wwd flags] to auto-kill after that long
   ```

The fake JVM answers Nexon SDK calls with a fixed local identity: npsn 1001, token `LOCAL_TEST_TOKEN`, region KR.

## Debug environment variables

Pass these with `--env K=V`, or export them before running `run-og.sh`.

| variable | effect |
|---|---|
| `WWD_VK_TRACE=1/2` | trace Vulkan calls (`2` = before each call is issued) |
| `WWD_CAPTURE=<frames>` | dump the listed presented frames to `capture_N.bmp` |
| `WWD_FONT_FIX=<frames>` | at the listed frames, repoint UI font faces at the system CJK font (`run-og.sh` sets this by default) |
| `WWD_TRACE_NET`, `WWD_TRACE_DNS` | socket and DNS tracing |
| `WWD_INPUT_TRACE` | touch/input event tracing |
| `WWD_NX_COUNTRY`, `WWD_NX_LOCALE`, `WWD_NX_AUTH_ENV` | override the Nexon SDK stand-in's region answers |

`--strace` logs every guest syscall. `--interp` disables the JIT.

## Layout

```
crates/wwd-cpu     A64 decoder, interpreter (+SIMD), x86-64 JIT
crates/wwd-linux   ELF loader, memory, syscalls, VFS, sockets, signals, host thunks (Vulkan, window, audio)
crates/wwd         CLI
droid/             guest-side Android libraries (Zig, built by droid/build.sh)
thunks/vulkan      generated guest Vulkan stubs (scripts/gen_vulkan_thunks.py)
guest/             AArch64 test programs
scripts/           image/APK tools, codegen, window helpers
```
