#!/usr/bin/env python3
"""Compact dump of all captured /play/upload samples: raw 0..32 hex + key fields.

Usage:
    python docs/rev/dump_samples.py [--uploads rev/uploads.txt]
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from decode_upload import decode_token, parse_plain  # noqa: E402


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--uploads", default=str(Path(__file__).resolve().parents[3] / "rev" / "uploads.txt"))
    args = ap.parse_args()

    for i, line in enumerate(Path(args.uploads).read_text(encoding="utf-8").splitlines()):
        if not line.strip():
            continue
        rec = json.loads(line)
        token = rec.get("token")
        if not token:
            continue
        out = decode_token(token)
        p = parse_plain(out["plain"])
        b = out["plain"][:92]
        print(f"#{i + 1} at={rec.get('at')} chart={p['chart_id']} mac_ok={out['mac_ok']}")
        print(f"   raw00..16 = {b[0:17].hex(' ')}")
        print(f"   raw17..32 = {b[17:33].hex(' ')}")
        print(f"   u32@17={p['unknown_u32_17']} u32@24={p['unknown_u32_24']} u32@88={p['unknown_u32_88']}")
        print(f"   f64@9={p['unknown_f64']!r}")
        print(f"   flags@21..23={p['flags_21_23']} = {bytes(p['flags_21_23']).hex(' ')}")
        print(f"   counts={p['counts']} max_combo={p['max_combo']} n={p['num_of_notes']} speed={p['speed_x100']}")


if __name__ == "__main__":
    main()
