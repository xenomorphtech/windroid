#!/usr/bin/env bash
# Build the wwd Android runtime ("droid") for aarch64 bionic with Zig.
# Output goes into the Android guest root: /wwd/lib64 and /wwd/bin.
set -euo pipefail
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")/.."
ROOT=${ROOT:-rootfs-android}
ZIG=tools/zig-x86_64-windows-0.17.1/zig.exe
H=tools/bionic-headers
B=$ROOT/apex/com.android.runtime/lib64/bionic
OUT_LIB=$ROOT/wwd/lib64
OUT_BIN=$ROOT/wwd/bin
mkdir -p "$OUT_LIB" "$OUT_BIN"

CFLAGS=(-target aarch64-linux-none -nostdlibinc
  -isystem $H/libc_include -isystem $H/libc_kernel_uapi -isystem $H/libc_kernel_uapi/asm-arm64
  -isystem $H/libc_kernel_android_uapi
  -D__ANDROID__ -D__ANDROID_API__=30 -D__ANDROID_MIN_SDK_VERSION__=30
  -fPIC -O2 -g -fno-stack-protector -Wall -Wno-unused-parameter -Wno-availability -Wno-unknown-attributes
  -Idroid)
LIBC=($B/libc.so $B/libm.so $B/libdl.so)

so() { # name, sources...
  local name=$1; shift
  "$ZIG" cc "${CFLAGS[@]}" -nostdlib -shared -Wl,-soname,$name -Wl,-z,max-page-size=4096 -Wl,--no-undefined-version \
    "$@" "${LIBC[@]}" -o "$OUT_LIB/$name"
  echo "  $name"
}

echo "droid: building into $ROOT/wwd"
so liblog.so droid/liblog.c
so libwwdjvm.so droid/jvm.c droid/jni_table.c droid/java_impl.c "$OUT_LIB/liblog.so"
so libandroid.so droid/libandroid.c "$OUT_LIB/libwwdjvm.so" "$OUT_LIB/liblog.so" "$ROOT/system/lib64/libz.so"
so libEGL.so droid/libEGL.c
so libGLESv3.so droid/libGLESv3.c
so libGLESv2.so droid/libGLESv3.c
so libOpenSLES.so droid/libOpenSLES.c
so libjnigraphics.so droid/libjnigraphics.c
so libvulkan.so thunks/vulkan/vk_stubs.c thunks/vulkan/vk_loader.c
so libnetd_client.so droid/libnetd_client.c
so libaaudio.so droid/libaaudio.c
so libvkquality.so droid/libvkquality.c
so libswappy.so droid/libswappy.c "$OUT_LIB/libvulkan.so"

"$ZIG" cc "${CFLAGS[@]}" -nostdlib -fPIE -pie guest/android/crt.c droid/launcher.c \
  "$OUT_LIB/libwwdjvm.so" "$OUT_LIB/libandroid.so" "$OUT_LIB/liblog.so" "${LIBC[@]}" \
  -Wl,--dynamic-linker=/system/bin/linker64 -Wl,-z,max-page-size=4096 -o "$OUT_BIN/wwd-app"
echo "  wwd-app"
