#!/usr/bin/env python3
import os, re, subprocess, sys

USAGE = "usage: check_win7_imports.py EXE [--verified FILE] [--write-verified FILE]"
NOT_ON_WIN7_DLLS = {
    "combase.dll", "shcore.dll", "dcomp.dll", "d3d12.dll", "dxcore.dll", "bcryptprimitives.dll",
    "windows.storage.dll", "twinapi.appcore.dll", "coremessaging.dll", "vcruntime140.dll",
    "vcruntime140_1.dll", "msvcp140.dll", "ucrtbase.dll",
}
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


def value_of(args, flag):
    if flag not in args:
        return None
    i = args.index(flag) + 1
    if i >= len(args) or args[i].startswith("--"):
        raise SystemExit("%s needs a file\n%s" % (flag, USAGE))
    return args[i]


def main():
    args = sys.argv[1:]
    if not args or not os.path.isfile(args[0]):
        print(USAGE)
        return 2
    exe = args[0]
    verified = None
    verified_path = value_of(args, "--verified")
    write_path = value_of(args, "--write-verified")
    if verified_path is not None:
        verified = {l.strip() for l in open(verified_path, encoding="utf-8") if l.strip() and not l.startswith("#")}
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
    if write_path is not None:
        path = write_path
        with open(path, "w", encoding="utf-8", newline="\n") as f:
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
