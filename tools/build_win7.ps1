# Build the Windows 7 (x64) exe and check its imports.
#
# Rust's own Windows targets need Windows 10 since Rust 1.78, so Windows 7 is built for
# the tier-3 target x86_64-win7-windows-msvc: a nightly compiler that rebuilds the
# standard library for it. No DirectX 12 there and no screen-reader layer (it needs
# combase.dll), so the app's default features are off: OpenGL only.
#
# The nightly is pinned: a tier-3 target has no guarantee of building on the next one.
# Change NIGHTLY deliberately, then run this and tools/check_win7_imports.py again.
#
# usage: powershell -File tools\build_win7.ps1        (from the repository's root)
$ErrorActionPreference = "Stop"
$NIGHTLY = "nightly-2026-09-25"
$TARGET = "x86_64-win7-windows-msvc"
$env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\abb-signal-spy-build-win7"

if (-not (rustup toolchain list | Select-String -SimpleMatch $NIGHTLY)) {
    rustup toolchain install $NIGHTLY --profile minimal --component rust-src
}
cargo "+$NIGHTLY" build --release -Z build-std=std,panic_unwind --target $TARGET -p abb-signal-spy --no-default-features
if ($LASTEXITCODE -ne 0) { throw "the Windows 7 build failed" }
$exe = "$env:CARGO_TARGET_DIR\$TARGET\release\abb-signal-spy.exe"
$env:PYTHONIOENCODING = "utf-8"
python tools\check_win7_imports.py $exe
if ($LASTEXITCODE -ne 0) { throw "the Windows 7 exe imports something Windows 7 does not have" }
Write-Output "Windows 7 exe: $exe"
