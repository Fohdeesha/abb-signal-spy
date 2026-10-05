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
