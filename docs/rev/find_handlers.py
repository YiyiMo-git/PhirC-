#!/usr/bin/env python3
"""定位控制流平坦化/VM 的 handler 区。

思路：VM handler 的典型特征 —— 每个小 stub 结尾都是「寄存器间接跳转」（br xN）或
「跳回分发器」（b <dispatcher>）。前面的表 dump 给出 93 个入口、跨度约 6KB，
所以这里扫描整个 .text，找一个 6KB 窗口里密集出现 br 的位置。

用法：python find_handlers.py <libphira.so> [跨度KB]
"""
import re
import subprocess
import sys
from pathlib import Path

OBJDUMP = Path(r"E:\tools\android-sdk\ndk\28.2.13676358\toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-objdump.exe")


def objdump(*args):
    return subprocess.run([str(OBJDUMP), *args], capture_output=True, text=True,
                          errors="replace").stdout.splitlines()


def main():
    so = sys.argv[1]
    span = int(sys.argv[2]) * 1024 if len(sys.argv) > 2 else 6 * 1024

    # 表里 93 个槽的低位偏移（相对最小值），用于后面校验 stub 起点
    offs = [0x0471, 0x04bd, 0x05c9, 0x06d5, 0x07ed, 0x0865, 0x09bd, 0x09d5, 0x0a2d,
            0x0a61, 0x0b69, 0x0bc9, 0x0c05, 0x0c29, 0x0c4d, 0x0c71, 0x0d8d, 0x0da5,
            0x0f29, 0x0f71, 0x0f9d, 0x0fc9, 0x13dd, 0x140d, 0x14a1, 0x15b1, 0x15dd,
            0x1649, 0x1a9d, 0x1aa5, 0x1aad, 0x1b01, 0x1b49, 0x1c1d]
    offs = sorted(set(offs))
    base_off = offs[0]
    rel = [o - base_off for o in offs]

    # 全量反汇编（只取 br / ret / b 的行，省内存）
    brs, terms = [], []
    for line in objdump("-d", so):
        m = re.match(r"\s*([0-9a-f]+):\s+[0-9a-f ]+\s+(\S+)\s*(.*)", line)
        if not m:
            continue
        pc, op, args = int(m.group(1), 16), m.group(2), m.group(3).strip()
        if op == "br":
            brs.append(pc)
        elif op == "ret":
            terms.append(pc)
    print(f"全模块：br {len(brs)} 处，ret {len(terms)} 处，跨度搜索窗口 {span // 1024} KB")

    # 滑动窗口统计 br 密度
    best = []
    for b in brs:
        lo, hi = b, b + span
        n = sum(1 for x in brs if lo <= x < hi)
        hits = sum(1 for r in rel if lo <= b + r < hi and any(b + r == y for y in brs))
        best.append((n, b, hits))
    best.sort(reverse=True)
    print("\nbr 最密集的 6KB 窗口（起始地址、窗口内 br 数）：")
    for n, b, hits in best[:5]:
        print(f"  0x{b:x}   br={n}   与表偏移吻合的入口 {hits}/{len(rel)}")

    # 用表偏移去验证最优窗口：看这些偏移处是不是 stub 起点（前一条指令是 ret 或 b）
    n, win, _ = best[0]
    term_set = set(terms)
    print(f"\n对最优窗口 0x{win:x} 校验（前一条指令是 ret 的比例）：")
    ok = 0
    for r in rel:
        addr = win + r
        if (addr - 4) in term_set:
            ok += 1
    print(f"  {ok}/{len(rel)} 个入口的前一条指令是 ret")
    print("\n该窗口前 24 条指令：")
    for line in objdump("-d", f"--start-address={hex(win)}", f"--stop-address={hex(win + 0x60)}", so)[4:28]:
        print("   ", line.strip())


if __name__ == "__main__":
    main()
