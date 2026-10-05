import re, subprocess
from pathlib import Path
OD = Path(r"E:\tools\android-sdk\ndk\28.2.13676358\toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-objdump.exe")
SO = r"E:\Code\Rust\Phigros\rev\libphira.so"
MASK = (1<<64)-1
def run(*a): return subprocess.run([str(OD),*a],capture_output=True,text=True,errors="replace").stdout.splitlines()
rel={}
for line in run("-R",SO):
    m=re.match(r"\s*([0-9a-f]{16})\s+R_AARCH64_RELATIVE\s+\*ABS\*([+-])0x([0-9a-f]+)",line)
    if m:
        v=int(m.group(3),16); rel[int(m.group(1),16)]=((-v)&MASK) if m.group(2)=="-" else v
syms=[]
for line in run("-T",SO):
    m=re.match(r"\s*([0-9a-f]{16})\s+\S+\s+\S+\s+([0-9a-f]{16})\s+(\S+)",line)
    if m: syms.append((int(m.group(1),16),int(m.group(2),16),m.group(3)))
def sym(a):
    for s,z,n in syms:
        if s<=a<s+z: return n
    return None
# 代码里实际用到的键族
KEYS=[0xB1C7970B2F87B284,0xB1C7970B2F87B294,0xB1C7970B2F87B29C,0xB1C7970B2F87B2A4,
      0xB1C7970B2F87B2AC,0xB1C7970B2F87B2B4,0xB1C7970B2F87B2BC,0xB1C7970B2F87B2C4,
      0xAEA4B5F5E34B5409,0xFFEA592105163A04]
print("槽地址        重定位值            用真键族解出（模块内）")
print("-"*74)
hit=0
for slot in range(0x18c2fe0,0x18c3400,8):
    if slot not in rel: continue
    base=rel[slot]; found=[]
    for k in KEYS:
        t=(base+k)&MASK
        if 0x1000<=t<0x1A00000: found.append((k,t))
    if found:
        hit+=1
        for k,t in found:
            print(f"{slot:#12x}  {base:#018x}  {t:#14x}  {sym(t) or '（数据/内部）'}  [键 {k:#018x}]")
print("-"*74)
print(f"共 {hit} 个槽被真键族解出")