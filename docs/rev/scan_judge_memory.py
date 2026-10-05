#!/usr/bin/env python3
"""Locate the live JudgeInner-like structure in the official Phira PC client.

Usage (after one real play, with the client still open):

    python docs/rev/scan_judge_memory.py \
        --process phira-main.exe \
        --counts 13 2 0 1385 \
        --max-combo 5 \
        --num-notes 1400

The counts/max_combo/num_notes values can be obtained from a captured upload
token via:

    python docs/rev/decode_upload.py --uploads rev/uploads.txt

This script only reads process memory with ReadProcessMemory; it does not
inject code, patch the client, or write to the target process.
"""
from __future__ import annotations

import argparse
import ctypes
import os
import struct
import sys
from ctypes import wintypes
from pathlib import Path

if sys.platform != "win32":
    print("This script must run on Windows.", file=sys.stderr)
    raise SystemExit(2)

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

PROCESS_VM_READ = 0x0010
PROCESS_QUERY_INFORMATION = 0x0400
PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
MEM_COMMIT = 0x1000
PAGE_NOACCESS = 0x01
PAGE_GUARD = 0x100
TH32CS_SNAPPROCESS = 0x00000002
INVALID_HANDLE_VALUE = ctypes.c_void_p(-1).value


class PROCESSENTRY32(ctypes.Structure):
    _fields_ = [
        ("dwSize", wintypes.DWORD),
        ("cntUsage", wintypes.DWORD),
        ("th32ProcessID", wintypes.DWORD),
        ("th32DefaultHeapID", ctypes.c_void_p),
        ("th32ModuleID", wintypes.DWORD),
        ("cntThreads", wintypes.DWORD),
        ("th32ParentProcessID", wintypes.DWORD),
        ("pcPriClassBase", ctypes.c_long),
        ("dwFlags", wintypes.DWORD),
        ("szExeFile", wintypes.CHAR * 260),
    ]


class SYSTEM_INFO(ctypes.Structure):
    _fields_ = [
        ("wProcessorArchitecture", wintypes.WORD),
        ("wReserved", wintypes.WORD),
        ("dwPageSize", wintypes.DWORD),
        ("lpMinimumApplicationAddress", ctypes.c_void_p),
        ("lpMaximumApplicationAddress", ctypes.c_void_p),
        ("dwActiveProcessorMask", ctypes.c_void_p),
        ("dwNumberOfProcessors", wintypes.DWORD),
        ("dwProcessorType", wintypes.DWORD),
        ("dwAllocationGranularity", wintypes.DWORD),
        ("wProcessorLevel", wintypes.WORD),
        ("wProcessorRevision", wintypes.WORD),
    ]


class MEMORY_BASIC_INFORMATION(ctypes.Structure):
    _fields_ = [
        ("BaseAddress", ctypes.c_ulonglong),
        ("AllocationBase", ctypes.c_ulonglong),
        ("AllocationProtect", wintypes.DWORD),
        ("PartitionId", wintypes.WORD),
        ("__pad0", wintypes.WORD),
        ("RegionSize", ctypes.c_ulonglong),
        ("State", wintypes.DWORD),
        ("Protect", wintypes.DWORD),
        ("Type", wintypes.DWORD),
        ("__pad1", wintypes.DWORD),
    ]


kernel32.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
kernel32.Process32First.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32)]
kernel32.Process32First.restype = wintypes.BOOL
kernel32.Process32Next.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32)]
kernel32.Process32Next.restype = wintypes.BOOL
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
kernel32.CloseHandle.restype = wintypes.BOOL
kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel32.OpenProcess.restype = wintypes.HANDLE
kernel32.GetCurrentProcessId.argtypes = []
kernel32.GetCurrentProcessId.restype = wintypes.DWORD
kernel32.GetSystemInfo.argtypes = [ctypes.POINTER(SYSTEM_INFO)]
kernel32.GetSystemInfo.restype = None
kernel32.VirtualQueryEx.argtypes = [wintypes.HANDLE, ctypes.c_void_p, ctypes.POINTER(MEMORY_BASIC_INFORMATION), ctypes.c_size_t]
kernel32.VirtualQueryEx.restype = ctypes.c_size_t
kernel32.ReadProcessMemory.argtypes = [wintypes.HANDLE, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
kernel32.ReadProcessMemory.restype = wintypes.BOOL


def find_pid_by_name(name: str) -> int:
    snap = kernel32.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    if snap == INVALID_HANDLE_VALUE:
        raise OSError(ctypes.get_last_error(), "CreateToolhelp32Snapshot failed")
    try:
        pe = PROCESSENTRY32()
        pe.dwSize = ctypes.sizeof(pe)
        if not kernel32.Process32First(snap, ctypes.byref(pe)):
            raise OSError(ctypes.get_last_error(), "Process32First failed")
        wanted = name.lower()
        while True:
            exe = bytes(pe.szExeFile).split(b"\0", 1)[0].decode("mbcs", "replace")
            if exe.lower() == wanted or os.path.basename(exe).lower() == wanted:
                return int(pe.th32ProcessID)
            if not kernel32.Process32Next(snap, ctypes.byref(pe)):
                break
    finally:
        kernel32.CloseHandle(snap)
    raise LookupError(f"process not found: {name}")


def open_process(pid: int) -> int:
    rights = PROCESS_VM_READ | PROCESS_QUERY_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION
    h = kernel32.OpenProcess(rights, False, pid)
    if not h:
        # Fall back to limited rights; some builds deny QUERY_INFORMATION but allow read.
        h = kernel32.OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not h:
        raise OSError(ctypes.get_last_error(), "OpenProcess failed (try running elevated)")
    return h


def read_memory(h: int, addr: int, size: int) -> bytes | None:
    if size <= 0:
        return b""
    buf = (ctypes.c_char * size)()
    got = ctypes.c_size_t(0)
    ok = kernel32.ReadProcessMemory(h, ctypes.c_void_p(addr), buf, size, ctypes.byref(got))
    if not ok:
        return None
    return buf.raw[: got.value]


def iter_readable_regions(h: int):
    si = SYSTEM_INFO()
    kernel32.GetSystemInfo(ctypes.byref(si))
    max_addr = int(si.lpMaximumApplicationAddress or 0x7FFFFFFFFFFF)
    addr = 0
    while addr < max_addr:
        mbi = MEMORY_BASIC_INFORMATION()
        if kernel32.VirtualQueryEx(h, ctypes.c_void_p(addr), ctypes.byref(mbi), ctypes.sizeof(mbi)) == 0:
            break
        base = int(mbi.BaseAddress)
        size = int(mbi.RegionSize)
        if size <= 0:
            break
        protect = int(mbi.Protect)
        state = int(mbi.State)
        readable = state == MEM_COMMIT and not (protect & PAGE_NOACCESS) and not (protect & PAGE_GUARD)
        if readable:
            yield base, size, protect
        nxt = base + size
        if nxt <= addr:
            break
        addr = nxt


def find_pattern(h: int, pattern: bytes, max_hits: int = 2000) -> list[int]:
    hits: list[int] = []
    overlap = len(pattern) - 1
    for base, size, _protect in iter_readable_regions(h):
        pos = 0
        tail = b""
        while pos < size:
            chunk = min(1 << 20, size - pos)  # 1 MiB
            data = read_memory(h, base + pos, chunk)
            if data is None:
                break
            hay = tail + data
            start = 0
            while True:
                idx = hay.find(pattern, start)
                if idx < 0:
                    break
                addr = base + pos - len(tail) + idx
                if addr >= base:
                    hits.append(addr)
                    if len(hits) >= max_hits:
                        return hits
                start = idx + 1
            tail = hay[-overlap:] if overlap else b""
            pos += chunk
    return hits


def find_all_u32(data: bytes, value: int) -> list[int]:
    pat = struct.pack("<I", value)
    out = []
    start = 0
    while True:
        idx = data.find(pat, start)
        if idx < 0:
            return out
        out.append(idx)
        start = idx + 1


def dump_window(h: int, center: int, before: int = 0x200, after: int = 0x400) -> tuple[int, bytes] | None:
    start = max(0, center - before)
    size = before + after
    data = read_memory(h, start, size)
    if data is None:
        return None
    return start, data


def annotate_u32(data: bytes, base_addr: int, rel0: int) -> list[str]:
    lines = []
    for off in range(0, len(data) - 3, 4):
        val = struct.unpack_from("<I", data, off)[0]
        if val in (0, 1) or 3 <= val < 1000000:
            lines.append(f"    +{off - rel0:+#06x} {base_addr + off:#018x}  u32 {val}")
    return lines


def parse_int_list(text: str) -> list[int]:
    parts = [p for p in text.replace(",", " ").split() if p]
    return [int(p, 0) for p in parts]


def main() -> int:
    ap = argparse.ArgumentParser(description="scan a live Phira process for JudgeInner-like data")
    ap.add_argument("--process", default="phira-main.exe", help="process image name (default: phira-main.exe)")
    ap.add_argument("--pid", type=lambda s: int(s, 0), help="explicit PID")
    ap.add_argument("--counts", nargs="+", help="four counts: perfect good bad miss (either four args or one quoted string)")
    ap.add_argument("--max-combo", type=lambda s: int(s, 0))
    ap.add_argument("--num-notes", type=lambda s: int(s, 0))
    ap.add_argument("--window-before", type=lambda s: int(s, 0), default=0x400)
    ap.add_argument("--window-after", type=lambda s: int(s, 0), default=0x800)
    ap.add_argument("--max-hits", type=int, default=20)
    ap.add_argument("--dump-dir", default=str(Path(__file__).resolve().parents[2] / "rev"), help="directory for raw window dumps")
    ap.add_argument("--self-test", action="store_true", help="test ReadProcessMemory against this Python process")
    args = ap.parse_args()

    if args.self_test:
        h = open_process(kernel32.GetCurrentProcessId())
        try:
            needle = b"PHIRA_SCAN_SELFTEST_0123456789ABCDEF" * 8
            buf = ctypes.create_string_buffer(needle)
            addr = ctypes.addressof(buf)
            got = read_memory(h, addr, len(needle))
            ok = got == needle
            print(f"selftest ReadProcessMemory: {'OK' if ok else 'FAIL'} addr={addr:#x}")
            regions = list(iter_readable_regions(h))
            print(f"selftest readable regions: {len(regions)}")
            return 0 if ok and regions else 1
        finally:
            kernel32.CloseHandle(h)

    if not args.counts:
        print("error: --counts P G B M is required", file=sys.stderr)
        return 2
    counts: list[int] = []
    for item in args.counts:
        counts.extend(parse_int_list(item))
    if len(counts) != 4:
        print(f"error: --counts must resolve to four ints, got {counts!r}", file=sys.stderr)
        return 2
    counts = [int(x) & 0xFFFFFFFF for x in counts]
    pattern = b"".join(struct.pack("<I", x) for x in counts)

    pid = args.pid or find_pid_by_name(args.process)
    h = open_process(pid)
    try:
        print(f"process: {args.process} pid={pid}")
        print(f"counts pattern: {counts} -> {pattern.hex()}")
        t0 = __import__("time").time()
        hits = find_pattern(h, pattern, max_hits=max(100, args.max_hits * 20))
        print(f"pattern hits: {len(hits)}  ({__import__('time').time() - t0:.1f}s)")
        if not hits:
            print("no candidate. Make sure the client is still on the result/upload screen and the counts are correct.")
            return 1

        mc_pat = struct.pack("<I", args.max_combo) if args.max_combo is not None else None
        nn_pat = struct.pack("<I", args.num_notes) if args.num_notes is not None else None

        ranked = []
        for addr in hits:
            win = dump_window(h, addr, args.window_before, args.window_after)
            if win is None:
                continue
            wstart, data = win
            rel = addr - wstart
            n_off = [o for o in range(0, len(data) - 3, 1) if nn_pat and data[o : o + 4] == nn_pat]
            m_off = [o for o in range(0, len(data) - 3, 1) if mc_pat and data[o : o + 4] == mc_pat]
            # Prefer windows where counts, max_combo and num_notes are close together.
            best_dist = 10**9
            for a in n_off:
                for b in m_off:
                    best_dist = min(best_dist, abs(a - rel) + abs(b - rel))
            # A plausible float64 (std/avg) in [0, 1) near the counts is a weak positive.
            f64_near = 0
            for o in range(max(0, rel - 0x100), min(len(data) - 8, rel + 0x100), 8):
                try:
                    v = struct.unpack_from("<d", data, o)[0]
                except Exception:
                    continue
                if 0.0 < v < 1.0:
                    f64_near += 1
            ranked.append((best_dist, -f64_near, addr, wstart, data, rel, n_off, m_off))
        ranked.sort(key=lambda x: (x[0], x[1]))
        print(f"candidates: {len(ranked)}")
        shown = 0
        for best_dist, neg_f64, addr, wstart, data, rel, n_off, m_off in ranked:
            if shown >= args.max_hits:
                break
            shown += 1
            print("=" * 78)
            print(f"candidate {shown}: counts @ {addr:#018x}; window {wstart:#018x}; rel={rel:#x}")
            print(f"  distance score={best_dist}  plausible_f64={-neg_f64}")
            if args.num_notes is not None:
                print(f"  num_notes({args.num_notes}) offsets: {[hex(o - rel) for o in n_off]}")
            if args.max_combo is not None:
                print(f"  max_combo({args.max_combo}) offsets: {[hex(o - rel) for o in m_off]}")
            if args.dump_dir:
                out_dir = Path(args.dump_dir)
                out_dir.mkdir(parents=True, exist_ok=True)
                out_path = out_dir / f"memory_{pid}_{addr:#x}.bin"
                out_path.write_bytes(data)
                print(f"  raw window dump: {out_path}")
            print("  u32 values at counts start (first 0x120 bytes):")
            for off in range(0, min(len(data) - 3, rel + 0x120), 4):
                val = struct.unpack_from("<I", data, off)[0]
                marker = ""
                if off == rel:
                    marker = "  <-- counts[0]"
                print(f"    rel {off - rel:+#06x}  {val:#010x} {val}{marker}")
        return 0
    finally:
        kernel32.CloseHandle(h)


if __name__ == "__main__":
    raise SystemExit(main())
