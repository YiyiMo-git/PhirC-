#!/usr/bin/env python3
"""第二阶段：用「大量 b 跳向同一个分发器」这个特征定位控制流平坦化的 handler 区。

第一次扫描失败的原因：最密集的 br 是 PLT 跳转桩（每个 PLT 条目以 br x17 结尾）。
平坦化的真正特征是「handler 结尾跳回同一个分发器」，即出现频率极高的同一个 b 目标。

用法：python find_dispatcher.py <libphira.so> [排除起始hex]
"""
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

OBJDUMP = Path(r"E:\tools\android-sdk\ndk\28.2.13676358\toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-objdump.exe")


def objdump(*args):
    return subprocess.run([str(OBJDUMP), *args], capture_output=True, text=True,
                          errors="replace").stdout.splitlines()


def main():
    so = sys.argv[1]
    plt_lo = int(sys.argv[2], 16) if len(sys.argv) > 2 else 0x17B0000

    jumps = []          # (pc, target) 普通 b
    brs = []            # br 的 pc
    for line in objdump("-d", so):
        m = re.match(r"\s*([0-9a-f]+):\s+[0-9a-f ]+\s+(\S+)\s*(.*)", line)
        if not m:
            continue
        pc, op, args = int(m.group(1), 16), m.group(2), m.group(3).strip()
        if pc >= plt_lo:      # 跳过 PLT/GOT
            continue
        if op == "b":
            mm = re.match(r"(0x[0-9a-f]+)", args)
            if mm:
                jumps.append((pc, int(mm.group(1), 16)))
        elif op == "br":
            brs.append(pc)

    print(f".text 内普通跳转 b {len(jumps)} 条，br {len(brs)} 条（已排除 >= {plt_lo:#x} 的 PLT 区）")
    cnt = Counter(t for _, t in jumps)
    print("\n被跳向次数最多的 8 个地址（候选分发器）：")
    for target, n in cnt.most_common(8):
        srcs = [pc for pc, t in jumps if t == target]
        print(f"  → 0x{target:x}   被 {n} 处跳向   来源范围 0x{min(srcs):x}..0x{max(srcs):x}")

    # 对第一名：看它的来源是否聚成 6KB 的一块（= handler 区）
    if cnt:
        target, n = cnt.most_common(1)[0]
        srcs = sorted(pc for pc, t in jumps if t == target)
        print(f"\n第一名的来源分布（每 1KB 一档）：")
        buckets = Counter(s // 1024 for s in srcs)
        for b, c in sorted(buckets.items()):
            if c >= 3:
                print(f"  0x{b*1024:x}–0x{b*1024+1023:x}   {c} 处")
        gaps = [srcs[i + 1] - srcs[i] for i in range(len(srcs) - 1)]
        if gaps:
            print(f"\n相邻来源间距：最小 {min(gaps)}  最大 {max(gaps)}  中位 {sorted(gaps)[len(gaps)//2]}")


if __name__ == "__main__":
    main()
