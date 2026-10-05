#!/usr/bin/env python3
"""Brute-force search: is the 3-byte field at offset 21..23 a linear function of known fields?

Samples: rev/uploads.txt (6 tokens). Known per sample: P,G,B,M,max_combo,n.
We search   v = (a*P + b*G + c*B + d*M + e*mc + f*n + g) mod MOD
for each target byte position, using the disassembly-derived structure
    byte = (value & ~1) | flag
so we also test value = byte // 2 (7-bit value) and value = byte.
"""
from __future__ import annotations

import json
import sys
from itertools import product
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from decode_upload import decode_token, parse_plain  # noqa: E402

UPLOADS = Path(__file__).resolve().parents[3] / "rev" / "uploads.txt"


def load():
    rows = []
    for line in UPLOADS.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        rec = json.loads(line)
        p = parse_plain(decode_token(rec["token"])["plain"])
        P, G, B, M = p["counts"]
        rows.append(
            {
                "P": P, "G": G, "B": B, "M": M,
                "mc": p["max_combo"], "n": p["num_of_notes"],
                "bytes": p["flags_21_23"],
            }
        )
    return rows


def search(rows, target_index, value_mode, mod, coef_range):
    """Try to fit target = a*P+b*G+c*B+d*M+e*mc+f*n+g (mod mod)."""
    obs = []
    for r in rows:
        b = r["bytes"][target_index]
        obs.append(b // 2 if value_mode == "half" else b)
    feats = []
    for r in rows:
        feats.append([r["P"], r["G"], r["B"], r["M"], r["mc"], r["n"]])

    hits = []
    rng = range(-coef_range, coef_range + 1)
    for coefs in product(rng, repeat=6):
        # solve g from sample 0, verify on the rest
        s0 = sum(c * f for c, f in zip(coefs, feats[0]))
        g = (obs[0] - s0) % mod
        ok = True
        for i in range(1, len(rows)):
            s = sum(c * f for c, f in zip(coefs, feats[i])) + g
            if s % mod != obs[i] % mod:
                ok = False
                break
        if ok:
            hits.append((coefs, g))
            if len(hits) >= 5:
                break
    return hits


def main() -> None:
    rows = load()
    print(f"samples={len(rows)}")
    for r in rows:
        print("  ", r)
    for pos, name in enumerate("byte21 byte22 byte23".split()):
        for mode, mod in (("half", 128), ("raw", 256)):
            hits = search(rows, pos, mode, mod, 4)
            tag = f"{name} value_mode={mode} mod={mod} coefrange=+-4"
            if hits:
                print(f"HIT {tag}:")
                for coefs, g in hits:
                    print(f"    aP={coefs[0]} bG={coefs[1]} cB={coefs[2]} dM={coefs[3]} emc={coefs[4]} fn={coefs[5]} g={g}")
            else:
                print(f"miss {tag}")


if __name__ == "__main__":
    main()
