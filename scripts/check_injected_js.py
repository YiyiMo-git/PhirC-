#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""检查 webui.rs 里注入到 WebView 的 JS 是否语法正确。

为什么需要它：
    注入脚本是 `BRIDGE_JS + ERROR_JS + ALERT_JS + (debug) + HOTSPOT_JS +
    SETTINGS_NAV_JS + RESPACK_NAV_JS` **拼成一整段**用
    `with_initialization_script` 注入的。**其中任何一处语法错误都会让整段
    解析失败** —— 桥不装、开始页触点不铺、`页面就绪` 不打日志，6 秒后看门狗
    判定「WebView 不可用」直接隐藏页面，用户看到的就是「点不动」。

    2026-10-18 就踩过一次：`associateReplay` 后面漏了个逗号。

用法：
    python scripts/check_injected_js.py            # 检查
    python scripts/check_injected_js.py -v         # 同时打印每个脚本大小

依赖：node（用于 `node --check`）。
"""

import io
import os
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WEBUI = os.path.join(ROOT, "phira", "src", "webui.rs")

# 注入顺序与 `init_js` 的 format! 一致（debug_js 只在 PHIRC_WEBUI_DEBUG 时注入，
# 它本身也是合法 JS，一起检查没坏处）。
INJECT_ORDER = [
    "BRIDGE_JS",
    "ERROR_JS",
    "ALERT_JS",
    "DEBUG_JS",
    "HOTSPOT_JS",
    "SETTINGS_NAV_JS",
    "RESPACK_NAV_JS",
]


def extract(src):
    """取出 `const NAME: &str = r#"…"#;` 的正文。"""
    out = {}
    for m in re.finditer(r'const\s+([A-Z0-9_]+)\s*:\s*&str\s*=\s*r#"(.*?)"#;', src, re.DOTALL):
        out[m.group(1)] = m.group(2)
    return out


def find_node():
    for cand in (
        shutil.which("node"),
        r"C:\Users\Administrator\.workbuddy-ai\binaries\node\versions\22.22.2-6\node.exe",
        r"C:\nvm4w\nodejs\node.exe",
    ):
        if cand and os.path.isfile(cand):
            return cand
    return None


def main():
    verbose = "-v" in sys.argv
    src = io.open(WEBUI, encoding="utf-8").read()
    blocks = extract(src)

    missing = [n for n in INJECT_ORDER if n not in blocks]
    if missing:
        print("!! 找不到这些注入脚本常量：", ", ".join(missing))
        return 2

    node = find_node()
    if not node:
        print("!! 没找到 node，无法做语法检查")
        return 2

    parts = []
    for name in INJECT_ORDER:
        body = blocks[name]
        if verbose:
            print("  %-18s %6d 字符" % (name, len(body)))
        parts.append("// ==== %s ====\n%s" % (name, body))

    js = "\n".join(parts)
    tmp = tempfile.NamedTemporaryFile("w", suffix=".js", delete=False, encoding="utf-8")
    try:
        tmp.write(js)
        tmp.close()
        proc = subprocess.run([node, "--check", tmp.name], capture_output=True, text=True, encoding="utf-8", errors="replace")
        if proc.returncode == 0:
            print("✅ 注入脚本语法正确（%d 个块，共 %d 字符）" % (len(parts), len(js)))
            return 0
        print("❌ 注入脚本有语法错误 —— 这会让整个 WebView 界面失联（桥/触点全没装）：\n")
        print((proc.stderr or "").strip())
        return 1
    finally:
        os.unlink(tmp.name)


if __name__ == "__main__":
    sys.exit(main())
