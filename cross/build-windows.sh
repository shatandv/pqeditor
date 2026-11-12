#!/usr/bin/env bash
# Cross-build pqeditor.exe for Windows from this Linux box.
# See AGENTS.md "Windows cross-build" for the one-time toolchain setup.
set -euo pipefail

TOOLCHAIN="$HOME/.local/llvm-mingw-20240619-ucrt-ubuntu-20.04-x86_64"
TARGET=x86_64-pc-windows-gnullvm
STATIC="$PWD/cross/static"

mkdir -p "$STATIC"
# libc++/libunwind static archives ONLY (no .dll.a twins), so the final
# link resolves -lc++/-lunwind statically -> self-contained exe.
# Re-copy after toolchain updates.
cp -f "$TOOLCHAIN/x86_64-w64-mingw32/lib/libc++.a" "$STATIC/"
cp -f "$TOOLCHAIN/x86_64-w64-mingw32/lib/libunwind.a" "$STATIC/"

export PATH="$TOOLCHAIN/bin:$PATH"
export CC_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang
export CXX_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang++

# cargo rustc (not plain cargo build) so the static-link args apply only to
# the final binary — keeps the dependency cache valid across iterations.
cargo rustc --release --target "$TARGET" -- \
    -C "link-arg=-L$STATIC" \
    -C "link-arg=-Wl,-Bstatic"

EXE="target/$TARGET/release/pqeditor.exe"
mkdir -p dist
cp -f "$EXE" dist/
cp -f test-data.parquet dist/

echo
echo "deliverables in dist/:"
ls -la dist/
