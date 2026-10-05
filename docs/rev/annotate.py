import json, re, subprocess
from pathlib import Path
OD = Path(r"E:\tools\android-sdk\ndk\28.2.13676358\toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-objdump.exe")
SO = r"E:\Code\Rust\Phigros\rev\libphira.so"
MASK=(1<<64)-1
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
KEYS=[0xB1C7970B2F87B284,0xB1C7970B2F87B294,0xB1C7970B2F87B29C,0xB1C7970B2F87B2A4,
      0xB1C7970B2F87B2AC,0xB1C7970B2F87B2B4,0xB1C7970B2F87B2BC,0xB1C7970B2F87B2C4,
      0xAEA4B5F5E34B5409,0x779FE40F101C93B0,0xDDEC255692D89CE9,0xF889F2AD7D8A94F0,
      0xFFEA592105163A04,0xFFEA59210C7E0C66,0xFFEA59210DAB84A1,0xFFEA59212D835E67,
      0xFFEA592183614214,0xFFEA5921CBD1C977,0xFFEA5921DBC06CD7,0xFFEA5921ECA305ED]
m={}
for slot in range(0x18c2f00,0x18c3500,8):
    if slot not in rel: continue
    base=rel[slot]
    for k in KEYS:
        t=(base+k)&MASK
        if 0x1000<=t<0x1A00000:
            m.setdefault(slot,{})[hex(k)]=t
Path(r"E:\Code\Rust\Phigros\rev\ptr_map.json").write_text(json.dumps(
    {"slots":len(m),"entries":sum(len(v) for v in m.values()),
     "map":{hex(k):{kk:vv for kk,vv in v.items()} for k,v in m.items()}},indent=1),encoding="utf-8")
code=[(t,s,k) for s,d in m.items() for k,t in d.items() if t<0x17B0000]
data=[(t,s,k) for s,d in m.items() for k,t in d.items() if t>=0x17B0000]
print(f"映射已生成：{len(m)} 个槽，{sum(len(v) for v in m.values())} 条 (槽,键)->地址")
print(f"  指向代码区: {len(code)} 条   指向数据/其他: {len(data)} 条")
names={}
for t,s,k in code:
    n=sym(t)
    if n: names.setdefault(n,[]).append(t)
print(f"  其中有导出符号的目标: {len(names)} 个符号")
for n,ts in list(names.items())[:14]:
    print(f"    {n}  ({len(ts)} 条, 例 0x{ts[0]:x})")
Path(r"E:\Code\Rust\Phigros\rev\ptr_map.txt").write_text(
    "\n".join(f"{s:#x} + {k} -> {t:#x}  {sym(t) or ''}" for s,d in sorted(m.items()) for k,t in sorted(d.items())),
    encoding="utf-8")
print("明细: rev\\ptr_map.txt")