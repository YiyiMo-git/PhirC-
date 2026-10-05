#!/usr/bin/env python3
"""dump 混淆间接表：对表里每个 8 字节槽，尝试用已知的不透明常量解出真实目标。

原理（见 上传成绩逆向笔记 9.1）：
    真实地址 = 槽的重定位值 + 不透明常量   (mod 2^64)
每个槽在链接期被写成 `真实目标 - 常量`，所以 addend 本身不像地址（巨大/负数）。

用法：
    python table.py <libphira.so> <表起始hex> <表结束hex>
例：
    python table.py libphira.so 0x18c2fe0 0x18c32c8
"""
import re
import subprocess
import sys
from pathlib import Path

OBJDUMP = Path(r"E:\tools\android-sdk\ndk\28.2.13676358\toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-objdump.exe")
MASK = (1 << 64) - 1
LO, HI = 0x1000, 0x1A00000

# 已从两个函数里提取到的不透明常量（>32 位）
CONSTS = [
    0x779FE40F101C93B0, 0xDDEC255692D89CE9, 0xF889F2AD7D8A94F0,
    0xFFEA592105163A04, 0xFFEA59210C7E0C66, 0xFFEA59210DAB84A1,
    0xFFEA59212D835E67, 0xFFEA592183614214, 0xFFEA5921CBD1C977,
    0xFFEA5921DBC06CD7, 0xFFEA5921ECA305ED,
    0xB1C7970B2F87B284, 0xB1C7970B2F87B294, 0xB1C7970B2F87B29C,
    0xB1C7970B2F87B2A4, 0xB1C7970B2F87B2AC, 0xB1C7970B2F87B2B4,
    0xB1C7970B2F87B2BC, 0xB1C7970B2F87B2C4, 0xAEA4B5F5E34B5409,
]


def objdump(*args):
    return subprocess.run([str(OBJDUMP), *args], capture_output=True, text=True,
                          errors="replace").stdout.splitlines()


def main():
    so, lo, hi = sys.argv[1], int(sys.argv[2], 16), int(sys.argv[3], 16)
    rel = {}
    for line in objdump("-R", so):
        m = re.match(r"\s*([0-9a-f]{16})\s+R_AARCH64_RELATIVE\s+\*ABS\*([+-])0x([0-9a-f]+)", line)
        if m:
            v = int(m.group(3), 16)
            rel[int(m.group(1), 16)] = ((-v) & MASK) if m.group(2) == "-" else v
    syms = []
    for line in objdump("-T", so):
        m = re.match(r"\s*([0-9a-f]{16})\s+\S+\s+\S+\s+([0-9a-f]{16})\s+(\S+)", line)
        if m:
            syms.append((int(m.group(1), 16), int(m.group(2), 16), m.group(3)))

    def sym(a):
        for s, size, n in syms:
            if s <= a < s + size:
                return n
        return None

    print(f"表 0x{lo:x}..0x{hi:x}  槽数 {(hi-lo)//8}")
    print(f"{'槽地址':>10}  {'重定位值':>18}  {'解出的目标':>12}  符号 / 判定")
    print("-" * 78)
    ok = 0
    for slot in range(lo, hi, 8):
        if slot not in rel:
            continue
        base = rel[slot]
        found = []
        for c in CONSTS:
            t = (base + c) & MASK
            if LO <= t < HI:
                found.append((c, t))
        if not found:
            print(f"{slot:#10x}  {base:#018x}  {'—':>12}  （无常量能解出模块内地址）")
            continue
        for c, t in found:
            n = sym(t)
            ok += 1
            print(f"{slot:#10x}  {base:#018x}  {t:#12x}  {n or '（内部/数据，无符号）'}   [常量 {c:#018x}]")
    print("-" * 78)
    print(f"解出 {ok} 个有效目标")


if __name__ == "__main__":
    main()
