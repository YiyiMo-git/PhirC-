#!/usr/bin/env python3
"""Reference re-implementation of the official `encode_record` (Phira /play/upload token).

Chain (see docs/上传成绩逆向笔记.md):
    record_92B
      -> custom_enc            (pad to (len & !0xf)+0x10 with len & 0xf, then per 16B block:
                                zeta subset-sum transform + 4-bit bit-reversal permutation)
      -> 96B ciphertext
      -> || HMAC-SHA256(KEY, ciphertext)          (32B)
      -> raw deflate (zlib, wbits=-15)
      -> base64

Usage:
    python docs/rev/encode_upload.py --verify            # byte-exact regression vs rev/uploads.txt
    python docs/rev/encode_upload.py --selftest          # tiny round-trip sanity check
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import struct
import sys
import zlib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from decode_upload import (  # noqa: E402
    _HMAC_KEY,
    _bitrev_permute,
    custom_enc,
    custom_enc_inverse,
    decode_token,
    parse_plain,
    zeta_transform,
)


def enc_blocks(data: bytes) -> bytes:
    """custom_enc's per-16-byte-block transform, without the length padding.

    Needed because custom_enc(96 bytes) would pad to 112; the captured ciphertext
    is exactly the transform of the 96-byte decrypted buffer.
    """
    out = bytearray()
    for off in range(0, len(data), 16):
        out += _bitrev_permute(zeta_transform(data[off : off + 16]))
    return bytes(out)

UPLOADS = Path(__file__).resolve().parents[3] / "rev" / "uploads.txt"

# --- constants recovered from libphira.so (see note section 19.5) -------------
VERSION = 3
CONST_28 = 0xD8
CONST_82 = 0x74
CONST_83 = 0xD2
PAD_FILL = None  # computed as len & 0xf


def build_record(
    player_id: int,
    chart_id: int,
    variance: float,
    max_combo: int,
    counts: tuple[int, int, int, int],
    speed_x100: int,
    chart_sha256: bytes,
    num_of_notes: int,
    u32_17: int = 0,
    flags_21_23: bytes = b"\x00\x00\x00",
    u32_24: int = 0,
    u32_88: int = 0,
) -> bytes:
    """Serialize the 92-byte record exactly as encode_record does."""
    assert len(chart_sha256) == 32
    assert len(flags_21_23) == 3
    out = bytearray()
    out += bytes([VERSION])                       # 0
    out += struct.pack("<I", player_id)           # 1..4   LE
    out += struct.pack(">I", chart_id)            # 5..8   BE  (rev w9)
    out += struct.pack("<d", variance)            # 9..16  f64 LE
    out += struct.pack("<I", u32_17)              # 17..20 (= [scene+0x288])
    out += bytes(flags_21_23)                     # 21..23
    out += struct.pack("<I", u32_24)              # 24..27 (global atomic)
    out += bytes([CONST_28])                      # 28
    out += struct.pack("<I", max_combo)           # 29..32
    out += struct.pack("<4I", *counts)            # 33..48
    out += bytes([speed_x100])                    # 49
    out += chart_sha256                           # 50..81
    out += bytes([CONST_82, CONST_83])            # 82, 83
    out += struct.pack("<I", num_of_notes)        # 84..87
    out += struct.pack("<I", u32_88)              # 88..91
    assert len(out) == 92, len(out)
    return bytes(out)


def make_payload(record: bytes) -> bytes:
    """custom_enc + HMAC-SHA256 -> 128 bytes (before deflate)."""
    cipher = custom_enc(record)
    return cipher + hmac.new(_HMAC_KEY, cipher, hashlib.sha256).digest()


def deflate_raw(data: bytes, level: int = 0) -> bytes:
    """Raw deflate.

    NOTE: the official client uses level 0 (stored / uncompressed blocks) --
    that is why the 128-byte payload becomes a 133-byte token.  Default to 0
    so our tokens are byte-identical to the official ones.
    """
    co = zlib.compressobj(level, zlib.DEFLATED, -15)
    return co.compress(data) + co.flush()


def encode_token(record: bytes, level: int = 0) -> str:
    return base64.b64encode(deflate_raw(make_payload(record), level)).decode()


# ---------------------------------------------------------------------------
def verify() -> int:
    """Rebuild every captured token from its own decoded fields and compare."""
    rows = []
    for line in UPLOADS.read_text(encoding="utf-8").splitlines():
        if line.strip():
            rows.append(json.loads(line))

    ok_payload = ok_token = 0
    level_hits: dict[int, int] = {}
    for i, rec in enumerate(rows, 1):
        dec = decode_token(rec["token"])
        p = parse_plain(dec["plain"])
        rebuilt = build_record(
            player_id=p["player_id"],
            chart_id=p["chart_id"],
            variance=p["unknown_f64"],
            max_combo=p["max_combo"],
            counts=tuple(p["counts"]),
            speed_x100=p["speed_x100"],
            chart_sha256=bytes.fromhex(p["chart_sha256"]),
            num_of_notes=p["num_of_notes"],
            u32_17=p["unknown_u32_17"],
            flags_21_23=bytes(p["flags_21_23"]),
            u32_24=p["unknown_u32_24"],
            u32_88=p["unknown_u32_88"],
        )
        same_plain = rebuilt == dec["plain"][:92]
        payload = make_payload(rebuilt)
        # dec["plain"] is the *decrypted* 96-byte buffer; re-encrypting it must give the ciphertext
        cipher_expected = enc_blocks(dec["plain"])
        same_payload = payload == cipher_expected + dec["mac"]
        token_match = False
        for lvl in range(0, 10):
            if encode_token(rebuilt, lvl) == rec["token"]:
                level_hits[lvl] = level_hits.get(lvl, 0) + 1
                token_match = True
        ok_payload += same_payload
        ok_token += token_match
        print(
            f"#{i} chart={p['chart_id']:>6} record={'OK' if same_plain else 'MISMATCH'}"
            f" payload={'OK' if same_payload else 'MISMATCH'}"
            f" token_by_level={'OK' if token_match else 'no-level-matched'}"
        )
    print(f"\nrecord+payload exact: {ok_payload}/{len(rows)}    token exact: {ok_token}/{len(rows)}")
    if level_hits:
        print("deflate levels that reproduced the captured bytes:", level_hits)
    else:
        print("deflate levels that reproduced the captured bytes: none (content still equivalent)")
    return 0 if ok_payload == len(rows) else 1


def selftest() -> int:
    rec = build_record(
        player_id=1651826,
        chart_id=46304,
        variance=0.006464875507526675,
        max_combo=135,
        counts=(182, 4, 0, 19),
        speed_x100=100,
        chart_sha256=hashlib.sha256(b"x").digest(),
        num_of_notes=205,
    )
    tok = encode_token(rec)
    back = decode_token(tok)
    assert back["mac_ok"], "HMAC mismatch"
    assert back["plain"][:92] == rec, "round-trip mismatch"
    print("selftest OK: round-trip through token reproduces the record byte-for-byte")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--verify", action="store_true", help="byte-exact regression against rev/uploads.txt")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.verify:
        return verify()
    if args.selftest:
        return selftest()
    ap.print_help()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
