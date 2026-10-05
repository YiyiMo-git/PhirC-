#!/usr/bin/env python3
"""从官方 Phigros 客户端里提取噪域（红区 / Noise Field）用的四张贴图。

跑完会在 `assets/blockarea/` 生成：

    BlockNoise1.png      256x256  R8      区域合成的位移噪声（只用 R 通道）
    PointNoise.png       128x128  R8      火花 / 噪点贴图（只用 R 通道）
    FD_Noise_00000.png   256x256  RGB565  噪域主噪声
    TouchHover.png        44x44   RGBA32  触摸悬停的模糊圆（源资源名 Round10_Blur4）

这几张是 `prpr/src/core/block_shader.rs` / `block_mask.rs` / `block_touch.rs` 运行时
从磁盘读的官方素材；缺任何一张，噪域就整体降级成一块纯红矩形。

## 约定（不要随手改，否则画面会上下颠倒或整体偏暗）

* 存盘方向 = 资源包原始行的**上下翻转**。渲染时 `tex()` 会再翻一次上传，
  这样才对齐 Unity GLES「V=0 在下方」的采样。
* R8 的两张只把数值写进 R 通道，G/B 保持 0——shader 与 CPU 遮罩都只读 `.r`。
* FD_Noise_00000 是 RGB565，按 GLES 的 UNorm 规则补低位
  `(b << s) | (b >> (8 - bits - s))`；只左移会让整张图偏暗。

## 用法

    pip install UnityPy            # 只在提取时需要，应用本身不依赖它
    python scripts/extract_blockarea_assets.py --apk /path/to/Phigros.apk
    python scripts/extract_blockarea_assets.py --tap    # 从 TapTap 取（只下载 176 MB）

`--tap` 用 TapTap 官方接口拿到 APK 直链，再用 HTTP Range 只下载
`assets/bin/Data/data.unity3d`（176 MB，未压缩存放），不拉整个 3.3 GB 安装包。
接口签名串取自公开仓库 7aGiven/Phigros_Resource 的 `taptap.py`。
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import struct
import sys
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

import numpy as np

REPO = Path(__file__).resolve().parents[1]
DEFAULT_OUT = REPO / "assets" / "blockarea"

PHIGROS_TAP_ID = 165287
UNITY_DATA = "assets/bin/Data/data.unity3d"
ASSET_NAME = {
    "BlockNoise1": "BlockNoise1.png",
    "PointNoise": "PointNoise.png",
    "FD_Noise_00000": "FD_Noise_00000.png",
    "Round10_Blur4": "TouchHover.png",
}
FMT_NAME = {4: "RGBA32", 7: "RGB565", 63: "R8"}


# ------------------------------------------------------------------ 纹理转换


def expand565(words: np.ndarray) -> np.ndarray:
    """RGB565 -> RGB8，按 GLES 的 UNorm 规则补低位。"""
    bits = np.stack([words >> 11, (words >> 5) & 63, words & 31], axis=-1)
    r = (bits[..., 0].astype(np.uint16) << 3) | (bits[..., 0] >> 2)
    g = (bits[..., 1].astype(np.uint16) << 2) | (bits[..., 1] >> 4)
    b = (bits[..., 2].astype(np.uint16) << 3) | (bits[..., 2] >> 2)
    return np.stack([r, g, b], axis=-1).astype(np.uint8)


def build_from_bundle(blob: bytes, out: Path) -> dict:
    import UnityPy
    from PIL import Image

    out.mkdir(parents=True, exist_ok=True)
    env = UnityPy.load(blob)
    report = {}
    for obj in env.objects:
        if obj.type.name != "Texture2D":
            continue
        data = obj.read()
        if data.m_Name not in ASSET_NAME:
            continue
        width, height, fmt = data.m_Width, data.m_Height, data.m_TextureFormat
        raw = np.frombuffer(data.get_image_data(), dtype=np.uint8)
        if fmt == 7:
            words = np.frombuffer(raw.tobytes(), dtype="<u2").reshape(height, width)
            arr = expand565(words)
        elif fmt == 63:
            channel = raw.reshape(height, width)
            arr = np.stack([channel, np.zeros_like(channel), np.zeros_like(channel)], axis=-1)
        elif fmt == 4:
            arr = raw.reshape(height, width, 4)
        else:
            raise SystemExit("未预期的纹理格式 %d（%s）" % (fmt, data.m_Name))
        image = Image.fromarray(np.flipud(arr), "RGBA" if fmt == 4 else "RGB")
        path = out / ASSET_NAME[data.m_Name]
        image.save(path)
        blob = path.read_bytes()
        report[path.name] = {
            "unity_name": data.m_Name,
            "size": [width, height],
            "unity_format": FMT_NAME.get(fmt, str(fmt)),
            "png_mode": image.mode,
            "bytes": len(blob),
            "sha256": hashlib.sha256(blob).hexdigest(),
        }
    missing = set(ASSET_NAME.values()) - set(report)
    if missing:
        raise SystemExit("资源包里没找到：%s" % ", ".join(sorted(missing)))
    return report


# -------------------------------------------------------------------- 远程 zip


class RemoteZip:
    """只用 HTTP Range 读一个远端 zip 的目录和指定条目。"""

    def __init__(self, url: str):
        self.url = url
        req = urllib.request.Request(url, method="HEAD", headers={"User-Agent": "Mozilla/5.0"})
        with urllib.request.urlopen(req, timeout=60) as response:
            self.size = int(response.headers["Content-Length"])
            if response.headers.get("Accept-Ranges", "") != "bytes":
                raise SystemExit("该地址不支持 Range，无法只取所需条目")
        self._entries = None

    def read(self, start: int, end: int) -> bytes:
        req = urllib.request.Request(
            self.url,
            headers={"Range": "bytes=%d-%d" % (start, end), "User-Agent": "Mozilla/5.0"},
        )
        with urllib.request.urlopen(req, timeout=300) as response:
            return response.read()

    def entries(self) -> dict:
        if self._entries is not None:
            return self._entries
        base = max(0, self.size - (1 << 22))
        tail = self.read(base, self.size - 1)
        n_total = cd_size = cd_off = 0
        eocd = tail.rfind(b"PK\x05\x06")
        if eocd < 0:
            raise SystemExit("没找到 zip 目录，可能不是标准 APK")
        n_total, cd_size, cd_off = struct.unpack_from("<HII", tail, eocd + 10)
        z64 = tail.rfind(b"PK\x06\x06")
        if z64 >= 0:
            n_total, cd_size, cd_off = struct.unpack_from("<QQQ", tail, z64 + 32)
        cd = tail[cd_off - base: cd_off - base + cd_size] if cd_off >= base else self.read(cd_off, cd_off + cd_size - 1)
        table = {}
        pos = 0
        while pos + 46 <= len(cd) and cd[pos:pos + 4] == b"PK\x01\x02":
            method = struct.unpack_from("<H", cd, pos + 10)[0]
            csize, usize = struct.unpack_from("<II", cd, pos + 20)
            nlen, elen, clen = struct.unpack_from("<HHH", cd, pos + 28)
            lho = struct.unpack_from("<I", cd, pos + 42)[0]
            name = cd[pos + 46:pos + 46 + nlen].decode("utf-8", "replace")
            table[name] = {"name": name, "method": method, "csize": csize, "usize": usize, "off": lho}
            pos += 46 + nlen + elen + clen
        if len(table) != n_total:
            print("  提示：目录条目 %d 条，EOCD 记 %d 条" % (len(table), n_total))
        self._entries = table
        return table

    def extract(self, name: str) -> bytes:
        entry = self.entries()[name]
        head = self.read(entry["off"], entry["off"] + 1023)
        nlen, elen = struct.unpack_from("<HH", head, 26)
        start = entry["off"] + 30 + nlen + elen
        blob = self.read(start, start + entry["csize"] - 1)
        if entry["method"] == 8:
            import zlib
            blob = zlib.decompress(blob, -15)
        if len(blob) != entry["usize"]:
            raise SystemExit("%s 解压后 %d B，期望 %d B" % (name, len(blob), entry["usize"]))
        return blob


# ---------------------------------------------------------------------- 取 APK


def from_apk(apk: Path, out: Path) -> dict:
    """本地 APK / XAPK：直接把 data.unity3d 读进内存解析（不落临时文件）。"""
    with zipfile.ZipFile(apk) as outer:
        source = outer
        if UNITY_DATA not in outer.namelist():  # XAPK：真身 apk 在压缩包里
            inner = next((n for n in outer.namelist() if n.endswith(".apk")), None)
            if inner is None:
                raise SystemExit("%s 里没有 %s，也没有内层 apk" % (apk, UNITY_DATA))
            source = zipfile.ZipFile(io.BytesIO(outer.read(inner)))
        if UNITY_DATA not in source.namelist():
            raise SystemExit("%s 里没有 %s" % (apk, UNITY_DATA))
        return build_from_bundle(source.read(UNITY_DATA), out)


TAP_X_UA = (
    "V=1&PN=TapTap&VN=2.40.1-rel.100000&VN_CODE=240011000&LOC=CN&LANG=zh_CN"
    "&CH=default&UID=%s&NT=1&SR=1080x2030&DEB=Xiaomi&DEM=Redmi+Note+5&OSV=9"
)
TAP_SECRET = "PeCkE6Fu0B10Vm9BKfPfANwCUAn5POcs"


def tap_download_url(app_id: int) -> tuple[str, str]:
    """返回 (APK 直链, 版本号)。"""
    import random
    import string
    import time
    import uuid
    from http.client import HTTPSConnection

    uid = uuid.uuid4()
    x_ua = TAP_X_UA % uid
    conn = HTTPSConnection("api.taptapdada.com", timeout=30)
    conn.request(
        "GET",
        "/app/v2/detail-by-id/%d?X-UA=%s" % (app_id, urllib.parse.quote(x_ua)),
        headers={"User-Agent": "okhttp/3.12.1"},
    )
    detail = json.load(conn.getresponse())["data"]["download"]
    apk_id = detail["apk_id"]
    version = detail["apk"].get("version_name", "?")

    nonce = "".join(random.sample(string.ascii_lowercase + string.digits, 5))
    param = (
        "abi=arm64-v8a,armeabi-v7a,armeabi&id=%d&node=%s&nonce=%s&sandbox=1"
        "&screen_densities=xhdpi&time=%s" % (apk_id, uid, nonce, int(time.time()))
    )
    sign = hashlib.md5(("X-UA=%s&%s%s" % (x_ua, param, TAP_SECRET)).encode()).hexdigest()

    conn2 = HTTPSConnection("api.taptapdada.com", timeout=30)
    conn2.request(
        "POST",
        "/apk/v1/detail?X-UA=" + urllib.parse.quote(x_ua),
        body=("%s&sign=%s" % (param, sign)).encode(),
        headers={"Content-Type": "application/x-www-form-urlencoded", "User-Agent": "okhttp/3.12.1"},
    )
    apk = json.load(conn2.getresponse())["data"]["apk"]
    return apk["download"], version


def from_tap(out: Path) -> dict:
    url, version = tap_download_url(PHIGROS_TAP_ID)
    print("TapTap Phigros %s，读取 zip 目录…" % version)
    remote = RemoteZip(url)
    blob = remote.extract(UNITY_DATA)
    print("  %s  %d B（未压缩存放）" % (UNITY_DATA, len(blob)))
    return build_from_bundle(blob, out)


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--apk", type=Path, help="本地 Phigros APK / XAPK")
    group.add_argument("--tap", action="store_true", help="从 TapTap 取（只下载 data.unity3d）")
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT, help="输出目录（默认 assets/blockarea）")
    args = parser.parse_args()

    report = from_tap(args.out) if args.tap else from_apk(args.apk, args.out)

    print("\n写入 %s" % args.out)
    for name, info in sorted(report.items()):
        print("  %-20s %dx%d %-7s %7d B  %s" % (
            name, info["size"][0], info["size"][1], info["unity_format"],
            info["bytes"], info["sha256"][:16],
        ))
    return 0


if __name__ == "__main__":
    sys.exit(main())
