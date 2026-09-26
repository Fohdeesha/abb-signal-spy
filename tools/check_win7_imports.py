#!/usr/bin/env python3
"""Check a built .exe's import table for anything Windows 7 SP1 (x64) does not have.

A Windows 7 build that imports one function Windows 7 lacks does not start at all
("entry point not found"), and a dependency update can bring one in silently. This
reads the import table with dumpbin (Visual Studio's) and refuses:

  * DLLs Windows 7 does not have (combase.dll, shcore.dll, dcomp.dll, ...) and any
    API-set import ("api-ms-win-*"): Windows 7 has only a few API sets, as stubs, and
    with the C runtime linked statically none should be imported;
  * functions that arrived in Windows 8 or later, from a list of the ones Rust's
    standard library and the window libraries are known to use;
  * with --verified FILE: any function not in that file, the import set of a build
    seen running on a real Windows 7 PC (made with --write-verified after such a test).

usage: check_win7_imports.py EXE [--verified FILE] [--write-verified FILE]
Exit 0: nothing refused. 1: refused imports (listed). 2: could not read the exe.
"""
import os, re, subprocess, sys

# Present in Windows 7 SP1 with no Windows 7-era updates needed, or absent there.
NOT_ON_WIN7_DLLS = {
    "combase.dll", "shcore.dll", "dcomp.dll", "d3d12.dll", "dxcore.dll", "bcryptprimitives.dll",
    "windows.storage.dll", "twinapi.appcore.dll", "coremessaging.dll", "vcruntime140.dll",
    "vcruntime140_1.dll", "msvcp140.dll", "ucrtbase.dll",
}
# Windows 8 or later; the ones known to be imported by Rust's std (for its Windows 10
# target), winit, wgpu, accesskit and friends when not loaded at run time.
WIN8_PLUS_FUNCTIONS = {
    "GetSystemTimePreciseAsFileTime", "WaitOnAddress", "WakeByAddressSingle", "WakeByAddressAll",
    "SetThreadDescription", "GetThreadDescription", "ProcessPrng", "CreateFile2",
    "GetDpiForWindow", "GetDpiForSystem", "GetDpiForMonitor", "SetProcessDpiAwareness",
    "SetProcessDpiAwarenessContext", "SetThreadDpiAwarenessContext", "GetThreadDpiAwarenessContext",
    "AdjustWindowRectExForDpi", "EnableNonClientDpiScaling", "GetSystemMetricsForDpi",
    "GetPointerType", "GetPointerInfo", "GetPointerTouchInfo", "GetPointerPenInfo",
    "GetPointerFrameInfo", "EnableMouseInPointer", "RegisterPointerDeviceNotifications",
    "UiaRaiseNotificationEvent", "UiaDisconnectProvider", "UiaDisconnectAllProviders",
    "CreateDXGIFactory2", "DXGIGetDebugInterface1", "VirtualAlloc2", "MapViewOfFile3",
    "SetDefaultDllDirectories", "AddDllDirectory", "RemoveDllDirectory", "GetCurrentPackageFullName",
    "GetCurrentPackageId", "CompareObjectHandles", "GetTempPath2W", "RoGetActivationFactory",
    "RoInitialize", "WindowsCreateString", "SetWindowDisplayAffinity2",
}


def dumpbin():
    vswhere = os.path.join(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)"), r"Microsoft Visual Studio\Installer\vswhere.exe")
    vs = subprocess.run([vswhere, "-latest", "-products", "*", "-property", "installationPath"], capture_output=True, text=True).stdout.strip()
    tools = os.path.join(vs, r"VC\Tools\MSVC")
    for ver in sorted(os.listdir(tools), reverse=True):
        p = os.path.join(tools, ver, r"bin\Hostx64\x64\dumpbin.exe")
        if os.path.isfile(p):
            return p
    raise SystemExit("dumpbin.exe not found (Visual Studio C++ build tools)")


def imports(exe):
    out = subprocess.run([dumpbin(), "/nologo", "/imports", exe], capture_output=True, text=True, errors="replace").stdout
    table, dll = {}, None
    for line in out.splitlines():
        # The section sizes after the imports ("  Summary", "  1000 .data") are not
        # functions.
        if line.strip() == "Summary":
            break
        m = re.match(r"^\s{4}(\S+\.(?:dll|DLL|drv))$", line)
        if m:
            dll = m.group(1).lower()
            table.setdefault(dll, set())
            continue
        m = re.match(r"^\s+[0-9A-F]+\s+(\S+)$", line)
        if dll and m:
            table[dll].add(m.group(1))
        m = re.match(r"^\s+Ordinal\s+(\d+)$", line)
        if dll and m:
            table[dll].add("#" + m.group(1))
    return table


def main():
    args = sys.argv[1:]
    if not args or not os.path.isfile(args[0]):
        print(__doc__)
        return 2
    exe = args[0]
    verified = None
    if "--verified" in args:
        path = args[args.index("--verified") + 1]
        verified = {l.strip() for l in open(path, encoding="utf-8") if l.strip() and not l.startswith("#")}
    table = imports(exe)
    if not table:
        print("no imports read from", exe)
        return 2
    refused = []
    for dll, funcs in sorted(table.items()):
        if dll in NOT_ON_WIN7_DLLS or dll.startswith("api-ms-win-") or dll.startswith("ext-ms-"):
            refused.append("%s (not on Windows 7, or not with these functions): %s" % (dll, ", ".join(sorted(funcs))))
            continue
        for f in sorted(funcs):
            if f in WIN8_PLUS_FUNCTIONS:
                refused.append("%s!%s (Windows 8 or later)" % (dll, f))
            elif verified is not None and "%s!%s" % (dll, f) not in verified:
                refused.append("%s!%s (not in the import set verified on Windows 7)" % (dll, f))
    total = sum(len(v) for v in table.values())
    print("%s: %d functions from %d DLLs" % (exe, total, len(table)))
    for dll, funcs in sorted(table.items()):
        print("  %-28s %d" % (dll, len(funcs)))
    if "--write-verified" in args:
        path = args[args.index("--write-verified") + 1]
        with open(path, "w", encoding="utf-8") as f:
            f.write("# Imports of a build seen running on Windows 7 SP1 x64; one DLL!function per line.\n")
            for dll, funcs in sorted(table.items()):
                for fn in sorted(funcs):
                    f.write("%s!%s\n" % (dll, fn))
        print("wrote", path)
    if refused:
        print("\nREFUSED: %d import(s) Windows 7 would not load:" % len(refused))
        for r in refused:
            print("  " + r)
        return 1
    print("\nnothing Windows 7 lacks, as far as this list knows")
    return 0


if __name__ == "__main__":
    sys.exit(main())
