#!/bin/sh
# Mechanical morning run (scheduled at 09:00 local via the background
# sleep in the session): verify the overnight WIP, and — only if the
# whole battery is green — build the exe and refresh dist/.
# Everything lands in /tmp/opencode/morning.log; NO push and NO wine
# run happen here, those need judgment and are done interactively.

cd /home/shata/projects/andrey/software/pqeditor || exit 1
echo "=== morning run start $(date)"
cargo fmt
echo "=== fmt done"
if cargo check --all-targets; then
    echo "=== check green"
else
    echo "=== CHECK FAILED — fix manually, skipping the rest"
    exit 1
fi
if cargo clippy --all-targets --quiet -- -D warnings; then
    echo "=== clippy green"
else
    echo "=== CLIPPY FAILED — fix manually, skipping the rest"
    exit 1
fi
if cargo test; then
    echo "=== test green"
else
    echo "=== TEST FAILED — fix manually, skipping the rest"
    exit 1
fi
export PATH="$HOME/.local/llvm-mingw-20240619-ucrt-ubuntu-20.04-x86_64/bin:$PATH"
export CC_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang
export CXX_x86_64_pc_windows_gnullvm=x86_64-w64-mingw32-clang++
if cargo rustc --release --target x86_64-pc-windows-gnullvm -- \
    -C "link-arg=-L$PWD/cross/static" -C "link-arg=-Wl,-Bstatic"
then
    cp target/x86_64-pc-windows-gnullvm/release/pqeditor.exe dist/pqeditor.exe
    echo "=== dist refreshed"
    md5sum dist/pqeditor.exe
    ls -la dist/pqeditor.exe
else
    echo "=== WINDOWS BUILD FAILED"
fi
wineserver -k 2>/dev/null || true
echo "=== morning run done $(date)"
