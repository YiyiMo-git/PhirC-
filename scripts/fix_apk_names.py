#!/usr/bin/env python3
"""修 aapt2 在 Windows 上的 zip 条目名编码缺陷，并重签 APK。

# 背景（2026-10-18 发现）

`aapt2` 在 Windows 上打 zip 时：entry name 的**字节是正确的 UTF-8**，但**漏置
UTF-8 flag（0x800）**。于是按规范读取方（Android 的 AssetManager、WebView 的
asset 加载、解压工具）会按 CP437 兜底解码 → 中文资源名全变乱码 →
页面里引用的 `背景.png` / `默认.png` / `收集品icon.png` 等在移动端 **全部 404**。

（中央目录/本地文件名字节其实是对的，只是缺 flag —— 所以不需要猜编码，
`zipfile` 重写一次让 Python 自动补 flag 即可。）

# 流程

1. **重写 zip**：逐条目重建（`orig_filename` 原始字节 → UTF-8 名），
   `zipfile.writestr` 对非 ASCII 名自动置 0x800 flag。compress_type /
   external_attr / date_time 原样保留（`.so` 仍是 stored）。
2. **zipalign**：`-p -f 4`（`.so` 按 4096 页对齐；必须先于签名）。
3. **apksigner 重签**：v1+v2+v3（zip 结构变了，旧签名失效）。

用法（由 build_apk.ps1 在 gradle assembleRelease 之后调用）：
    python fix_apk_names.py <apk> <build-tools-dir> <keystore> <ks-pass> <key-pass> <alias>
"""
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile


def log(msg: str) -> None:
    print(msg, flush=True)


def rewrite_zip(src: str, dst: str) -> int:
    """重建 zip，给非 ASCII 条目名补 UTF-8 flag。返回修复的条目数。"""
    fixed = 0
    with zipfile.ZipFile(src) as zin, zipfile.ZipFile(dst, "w") as zout:
        for info in zin.infolist():
            data = zin.read(info.filename)
            # orig_filename 是 zipfile 按 CP437 兜底解码的展示名（我们的 APK
            # 里原始字节是正确的 UTF-8，只是没打 flag）→ CP437 还原字节 → UTF-8 解码。
            # Python 的 cp437 codec 全 256 字节可往返，不抛错。
            try:
                name = info.orig_filename.encode("cp437").decode("utf-8")
            except UnicodeDecodeError:
                # 极端情况：字节不是 UTF-8（正常不会发生），保持展示名
                name = info.filename
            ni = zipfile.ZipInfo(name, date_time=info.date_time)
            ni.compress_type = info.compress_type
            ni.external_attr = info.external_attr
            ni.internal_attr = info.internal_attr
            ni.create_system = info.create_system
            # writestr 对非 ASCII 名自动置 0x800 flag —— 这就是修复点
            zout.writestr(ni, data)
            if not name.isascii():
                fixed += 1
    return fixed


def run(cmd: list) -> None:
    log("  $ " + " ".join(cmd))
    r = subprocess.run(cmd)
    if r.returncode != 0:
        raise SystemExit(f"命令失败（exit {r.returncode}）：{' '.join(cmd)}")


def main() -> None:
    if len(sys.argv) != 7:
        raise SystemExit(__doc__)
    apk, build_tools, keystore, ks_pass, key_pass, alias = sys.argv[1:7]

    zipalign = os.path.join(build_tools, "zipalign.exe")
    apksigner = os.path.join(build_tools, "apksigner.bat")
    for tool in (zipalign, apksigner, keystore):
        if not os.path.exists(tool):
            raise SystemExit(f"找不到 {tool}")

    log(f"[fix-apk] 处理 {apk}（{os.path.getsize(apk)/1048576:.1f} MB）")

    with tempfile.TemporaryDirectory() as td:
        fixed_zip = os.path.join(td, "fixed.apk")
        aligned = os.path.join(td, "aligned.apk")
        signed = os.path.join(td, "signed.apk")

        n = rewrite_zip(apk, fixed_zip)
        log(f"[fix-apk] 重写 zip：{n} 个非 ASCII 条目名补 UTF-8 flag")

        run([zipalign, "-p", "-f", "4", fixed_zip, aligned])
        # apksigner：alias 走 --ks-key-alias（不是位置参数），--out 指定输出
        run([
            apksigner, "sign",
            "--ks", keystore,
            "--ks-pass", f"pass:{ks_pass}",
            "--key-pass", f"pass:{key_pass}",
            "--ks-key-alias", alias,
            "--out", signed,
            aligned,
        ])
        run([apksigner, "verify", "--verbose", signed])
        shutil.move(signed, apk)

    log(f"[fix-apk] 完成：{apk}（{os.path.getsize(apk)/1048576:.1f} MB）")


if __name__ == "__main__":
    main()
