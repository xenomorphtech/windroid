#!/usr/bin/env bash
# Build the runtime and the AArch64 guest tests, then run the suite under both
# the interpreter and the JIT and require identical, passing output.
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
ZIG=tools/zig-x86_64-windows-0.17.1/zig.exe

cargo build --release
cargo test --release -p wwd-cpu --quiet

mkdir -p guest/out
for p in hello tests bench; do
    "$ZIG" cc -target aarch64-linux-musl -static -O2 "guest/$p.c" -o "guest/out/$p" -lm
done

W="$PWD/target/release/wwd.exe"
for p in hello tests bench; do
    "$W" --scan "guest/out/$p" | tail -1
done

cd guest
"$W" --interp out/tests > out/tests.interp.txt
"$W" out/tests > out/tests.jit.txt
diff out/tests.interp.txt out/tests.jit.txt
tail -1 out/tests.jit.txt
echo "interpreter and JIT agree"
"$W" out/bench 2000000
