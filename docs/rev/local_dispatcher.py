import re, subprocess, sys
from collections import Counter
from pathlib import Path
OD = Path(r"E:\tools\android-sdk\ndk\28.2.13676358\toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-objdump.exe")
SO = r"E:\Code\Rust\Phigros\rev\libphira.so"
def dis(a, n):
    out = subprocess.run([str(OD), "-d", f"--start-address={hex(a)}", f"--stop-address={hex(a+n)}", SO],
                         capture_output=True, text=True, errors="replace").stdout.splitlines()
    r = []
    for line in out:
        m = re.match(r"\s*([0-9a-f]+):\s+[0-9a-f ]+\s+(\S+)\s*(.*)", line)
        if m: r.append((int(m.group(1),16), m.group(2), m.group(3).strip()))
    return r
for name, a, n in [("custom_enc", 0xd24104, 0x75c), ("encode_record", 0xd24860, 0x18b8)]:
    ins = dis(a, n)
    bt = Counter(); brt = 0; brn = 0
    for pc, op, args in ins:
        if op == "b":
            m = re.match(r"(0x[0-9a-f]+)", args)
            if m: bt[int(m.group(1),16)] += 1
        elif op == "br": brn += 1
        elif op == "ret": brt += 1
    print(f"== {name} (0x{a:x}, {len(ins)} 条指令)  ret={brt}  br={brn}  内部 b 目标 {len(bt)} 个 ==")
    for t, c in bt.most_common(6):
        inside = a <= t < a+n
        print(f"   → 0x{t:x}  ×{c}  {'（函数内部）' if inside else ''}")
    # 打印函数内所有 b 目标落在 0xd24000-0xd27000 一带的（可能是 handler 区）
    zone = sorted({t for t in bt if 0xd24000 <= t < 0xd28000})
    if zone:
        print(f"   落在 0xd24000..0xd28000 的跳转目标 {len(zone)} 个: " + " ".join(hex(t) for t in zone[:12]) +
              (" …" if len(zone) > 12 else ""))