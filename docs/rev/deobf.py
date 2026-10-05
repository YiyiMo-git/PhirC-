#!/usr/bin/env python3
"""解开 libphira.so 里的 OLLVM 式指针混淆（用于逆向 inner 模块的 encode_record / custom_enc）。

混淆规则（已由人工验证两次）：
    真实地址 = 重定位表里的值 + 不透明常量   (mod 2^64)

用法：
    python deobf.py <libphira.so> <函数起始地址hex> <函数大小hex>
例：
    python deobf.py libphira.so 0xd24104 0x75c      # custom_enc
    python deobf.py libphira.so 0xd24860 0x18b8     # encode_record

依赖：本机 Android NDK 里的 llvm-objdump（跨架构、能反汇编 aarch64）。
"""
import re
import subprocess
import sys
from pathlib import Path

OBJDUMP = Path(r"E:\tools\android-sdk\ndk\28.2.13676358\toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-objdump.exe")
MASK = (1 << 64) - 1
# 模块内的合法地址范围（.so 大约 26MB）
LO, HI = 0x1000, 0x1A00000


def objdump(*args):
    return subprocess.run([str(OBJDUMP), *args], capture_output=True, text=True,
                          errors="replace").stdout.splitlines()


def parse_relocations(so):
    """槽地址 -> 真实值（R_AARCH64_RELATIVE 的 addend，可能是负数）"""
    rel = {}
    for line in objdump("-R", so):
        m = re.match(r"\s*([0-9a-f]{16})\s+R_AARCH64_RELATIVE\s+\*ABS\*([+-])0x([0-9a-f]+)", line)
        if m:
            addr = int(m.group(1), 16)
            val = int(m.group(3), 16)
            if m.group(2) == "-":
                val = (-val) & MASK
            rel[addr] = val
    return rel


def parse_symbols(so):
    syms = []
    for line in objdump("-T", so):
        m = re.match(r"\s*([0-9a-f]{16})\s+\S+\s+\S+\s+([0-9a-f]{16})\s+(\S+)", line)
        if m:
            syms.append((int(m.group(1), 16), int(m.group(2), 16), m.group(3)))
    return syms


def covering_symbol(syms, addr):
    for a, size, name in syms:
        if a <= addr < a + size:
            return name
    return None


def disasm(so, start, size):
    return objdump("-d", f"--start-address={hex(start)}", f"--stop-address={hex(start + size)}", so)


def main():
    so, start, size = sys.argv[1], int(sys.argv[2], 16), int(sys.argv[3], 16)
    rel = parse_relocations(so)
    syms = parse_symbols(so)
    regs = {}      # 寄存器 -> 常量（movz/movk 链）
    adrp = {}      # 寄存器 -> adrp 基址
    got = {}       # 寄存器 -> 该寄存器当前装的是哪个 GOT 槽的**值**
    hidden = []    # (偏移, 槽, 常量, 目标地址)

    lines = disasm(so, start, size)
    for line in lines:
        m = re.match(r"\s*([0-9a-f]+):\s+[0-9a-f ]+\s+(\S+)\s*(.*)", line)
        if not m:
            continue
        pc, op, args = int(m.group(1), 16), m.group(2), m.group(3)

        # 常量装载
        mm = re.match(r"(x\d+),\s*#(-?0x[0-9a-f]+)(?:,\s*lsl #(\d+))?", args)
        if op in ("mov", "movz") and mm:
            regs[mm.group(1)] = (int(mm.group(2), 16) << int(mm.group(3) or 0)) & MASK
            continue
        if op == "movk" and mm:
            r = mm.group(1)
            if r in regs:
                regs[r] |= (int(mm.group(2), 16) << int(mm.group(3) or 0)) & MASK
            continue
        # adrp 基址
        ma = re.match(r"(x\d+),\s*(0x[0-9a-f]+)", args)
        if op == "adrp" and ma:
            adrp[ma.group(1)] = int(ma.group(2), 16)
            continue
        # GOT 取数：ldr xD, [xS, #imm]
        mg = re.match(r"(x\d+),\s*\[(x\d+),\s*#(0x[0-9a-f]+)\]", args)
        if op == "ldr" and mg:
            d, s, off = mg.group(1), mg.group(2), int(mg.group(3), 16)
            if s in adrp:
                slot = adrp[s] + off
                got[d] = slot
            continue
        # 隐藏寻址：ldr xD, [xS, xI]  → 目标 = 槽的值 + 常量
        mh = re.match(r"(x\d+),\s*\[(x\d+),\s*(x\d+)\]", args)
        if op == "ldr" and mh:
            d, s, idx = mh.group(1), mh.group(2), mh.group(3)
            if s in got and idx in regs:
                slot = got[s]
                if slot in rel:
                    tgt = (rel[slot] + regs[idx]) & MASK
                    hidden.append((pc, slot, regs[idx], tgt))
            continue

    print(f"函数 0x{start:x}..0x{start+size:x}  指令行 {len(lines)}")
    print(f"不透明常量(>32位): {sum(1 for v in regs.values() if v > 0xffffffff)} 个")
    print(f"GOT 槽: {len(set(got.values()))} 个   隐藏寻址: {len(hidden)} 处")
    print("-" * 78)
    seen = set()
    for pc, slot, const, tgt in hidden:
        if tgt in seen:
            continue
        seen.add(tgt)
        inside = LO <= tgt < HI
        name = covering_symbol(syms, tgt) if inside else None
        tag = name or ("模块内(内部函数/数据)" if inside else "★ 不在模块内 —— 规则不符，需另行分析")
        print(f"  {pc:#08x}: 槽 {slot:#x} + {const:#018x} → {tgt:#x}   {tag}")


if __name__ == "__main__":
    main()
