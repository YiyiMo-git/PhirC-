#!/usr/bin/env python3
"""One-off check: does probe_upload's request builder reproduce the captured token exactly?

For each sample, rebuild with the OBSERVED flags (variant=keep) and compare the
token string with the one in rev/uploads.txt.  Must be identical byte-for-byte.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from decode_upload import decode_token, parse_plain  # noqa: E402
from encode_upload import build_record, encode_token  # noqa: E402

UPLOADS = HERE.parents[2] / "rev" / "uploads.txt"


def main() -> int:
    rows = [json.loads(l) for l in UPLOADS.read_text(encoding="utf-8").splitlines() if l.strip()]
    bad = 0
    for i, rec in enumerate(rows, 1):
        p = parse_plain(decode_token(rec["token"])["plain"])
        record = build_record(
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
        same = encode_token(record) == rec["token"]
        bad += not same
        print(f"#{i} chart={p['chart_id']:>6} token_identical={same}")
    print(f"\nidentical: {len(rows) - bad}/{len(rows)}")
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
