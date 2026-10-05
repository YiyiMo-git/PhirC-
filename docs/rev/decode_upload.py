#!/usr/bin/env python3
"""Decode / verify a captured Phira /play/upload token.

Pipeline (see docs/上传成绩逆向笔记.md):
    token = base64(
        raw_deflate(
            custom_enc(record_92B + 0x0c padding)
            || HMAC-SHA256(KEY, custom_enc_output)
        )
    )

KEY is obfuscated in libphira.so, decoded to the ASCII constant below.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import struct
import zlib
from pathlib import Path

# obfuscated 26-byte blob in libphira.so at 0x320074, deobfuscated by XOR constants
_HMAC_KEY = bytes.fromhex("4b5443664c65753664366771393537574f45796a2d6c69676874")
assert _HMAC_KEY == b"KTCfLeu6d6gq957WOEyj-light"

_MASK = 0xFF


def _rev4(i: int) -> int:
    r = 0
    for _ in range(4):
        r = (r << 1) | (i & 1)
        i >>= 1
    return r


def _bitrev_permute(data: bytes) -> bytes:
    a = list(data)
    for i in range(16):
        j = _rev4(i)
        if i < j:
            a[i], a[j] = a[j], a[i]
    return bytes(a)


def zeta_transform(block: bytes) -> bytes:
    a = list(block)
    for bit in range(4):
        step = 1 << bit
        for i in range(16):
            if i & step:
                a[i] = (a[i] + a[i ^ step]) & _MASK
    return bytes(a)


def mobius_transform(block: bytes) -> bytes:
    a = list(block)
    for bit in range(3, -1, -1):
        step = 1 << bit
        for i in range(16):
            if i & step:
                a[i] = (a[i] - a[i ^ step]) & _MASK
    return bytes(a)


def custom_enc(plain: bytes) -> bytes:
    """Forward custom_enc. Pads to (len & !0xf) + 0x10 with (len & 0xf)."""
    n = len(plain)
    pad = n & 0xF
    data = bytearray(plain)
    data.extend([pad] * (((n & ~0xF) + 0x10) - n))

    out = bytearray()
    for off in range(0, len(data), 16):
        block = bytes(data[off : off + 16])
        out += _bitrev_permute(zeta_transform(block))
    return bytes(out)


def custom_enc_inverse(cipher: bytes) -> bytes:
    """Inverse custom_enc for a multiple-of-16 ciphertext."""
    assert len(cipher) % 16 == 0
    out = bytearray()
    for off in range(0, len(cipher), 16):
        block = cipher[off : off + 16]
        out += mobius_transform(_bitrev_permute(block))
    return bytes(out)


def decode_token(token_b64: str):
    packed = base64.b64decode(token_b64)
    payload = zlib.decompress(packed, -15)  # raw deflate
    if len(payload) < 32:
        raise ValueError("payload too short")
    enc_part, mac = payload[:-32], payload[-32:]
    if len(enc_part) % 16:
        raise ValueError("custom_enc output length is not a multiple of 16")
    plain = custom_enc_inverse(enc_part)
    mac_ok = hmac.new(_HMAC_KEY, enc_part, hashlib.sha256).digest() == mac
    return {
        "packed_len": len(packed),
        "payload_len": len(payload),
        "enc_len": len(enc_part),
        "plain": plain,
        "mac": mac,
        "mac_ok": mac_ok,
        "hmac_expected": hmac.new(_HMAC_KEY, enc_part, hashlib.sha256).hexdigest(),
    }


def parse_plain(plain: bytes):
    """Parse the known part of the 92-byte record. Unknown fields are kept raw."""
    if len(plain) < 92:
        raise ValueError("plain record shorter than 92 bytes")
    b = plain[:92]
    o = {}
    o["version"] = b[0]
    o["player_id"] = struct.unpack_from("<I", b, 1)[0]
    o["chart_id"] = struct.unpack_from(">I", b, 5)[0]
    o["unknown_f64"] = struct.unpack_from("<d", b, 9)[0]
    o["unknown_u32_17"] = struct.unpack_from("<I", b, 17)[0]
    o["flags_21_23"] = list(b[21:24])
    o["unknown_u32_24"] = struct.unpack_from("<I", b, 24)[0]
    o["const_28"] = b[28]
    o["max_combo"] = struct.unpack_from("<I", b, 29)[0]
    o["counts"] = list(struct.unpack_from("<4I", b, 33))
    if sum(o["counts"]) != struct.unpack_from("<I", b, 84)[0]:
        pass  # may differ for malformed/short charts
    o["speed_x100"] = b[49]
    o["chart_sha256"] = b[50:82].hex()
    o["const_82"] = b[82]
    o["const_83"] = b[83]
    o["num_of_notes"] = struct.unpack_from("<I", b, 84)[0]
    o["unknown_u32_88"] = struct.unpack_from("<I", b, 88)[0]
    o["padding"] = b[92:96].hex()
    return o


def computed_score(counts, max_combo: int, num_of_notes: int) -> int:
    """Reference formula from prpr/src/judge.rs (no_combo_score=False)."""
    p, g, b, m = counts
    if p == num_of_notes:
        return 1_000_000
    acc = (p + 0.65 * g) / num_of_notes
    return round((0.9 * acc + max_combo / num_of_notes * 0.1) * 1_000_000)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("token", nargs="?", help="base64 token; if omitted, decode rev/uploads.txt")
    ap.add_argument("--uploads", default=str(Path(__file__).resolve().parents[2] / "rev" / "uploads.txt"))
    ap.add_argument("--chart-file", help="optional chart file path to verify chart_sha256")
    ap.add_argument("--last", action="store_true", help="only decode the last record in --uploads")
    args = ap.parse_args()

    records = []
    if args.token:
        records.append({"token": args.token})
    else:
        for line in Path(args.uploads).read_text(encoding="utf-8").splitlines():
            if line.strip():
                records.append(json.loads(line))
        if args.last and records:
            records = records[-1:]

    chart_hash = None
    if args.chart_file:
        chart_hash = hashlib.sha256(Path(args.chart_file).read_bytes()).hexdigest()

    for rec in records:
        token = rec.get("token") or rec.get("body", {}).get("token")
        if not token:
            continue
        out = decode_token(token)
        p = parse_plain(out["plain"])
        print("=" * 78)
        print("time:", rec.get("at", ""))
        print("packed:", out["packed_len"], "decompressed:", out["payload_len"], "custom_enc:", out["enc_len"])
        print("hmac_ok:", out["mac_ok"])
        print("version:", p["version"])
        print("player_id:", p["player_id"])
        print("chart_id:", p["chart_id"], "expected chart:", rec.get("chart"))
        print("unknown_f64:", p["unknown_f64"])
        print("unknown_u32_17:", p["unknown_u32_17"])
        print("flags_21_23:", [hex(x) for x in p["flags_21_23"]])
        print("unknown_u32_24:", p["unknown_u32_24"])
        print("const_28:", hex(p["const_28"]))
        print("max_combo:", p["max_combo"])
        print("counts [P,G,B,M]:", p["counts"], "sum:", sum(p["counts"]))
        print("num_of_notes:", p["num_of_notes"])
        print("speed_x100:", p["speed_x100"])
        print("chart_sha256:", p["chart_sha256"])
        if chart_hash:
            print("chart_sha256_ok:", chart_hash == p["chart_sha256"])
        print("const_82/83:", hex(p["const_82"]), hex(p["const_83"]))
        print("unknown_u32_88:", p["unknown_u32_88"])
        print("padding:", p["padding"])
        print("computed_score:", computed_score(p["counts"], p["max_combo"], p["num_of_notes"]))
        print("tail32:", out["mac"].hex())


if __name__ == "__main__":
    main()
