# Static C++ runtime for the Windows build

`libc++.a` and `libunwind.a` are copied from the llvm-mingw toolchain
(https://github.com/mstorsjo/llvm-mingw, LLVM/libc++ build) and committed
here so `cross/build-windows.sh` works without the exact toolchain
version present.

They are statically linked into `pqeditor.exe` only. License:
libc++/libunwind are dual-licensed under the Apache-2.0 license with the
LLVM exception (see llvm-project LICENSE.TXT). Refresh them from a
llvm-mingw release when bumping the toolchain.
