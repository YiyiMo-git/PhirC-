#!/usr/bin/env python3
"""Validate the variance model for record offset 9..16.

Model (derived from prpr/src/judge.rs:898-908 plus the disassembly at 0xd24dc8):

    Every judgement calls JudgeInner::commit(what, diff) with
        diff = 0.25                            if the judgement is Miss
        diff = 0.0                             if the note is Drag/Flick (synthetic)
        diff = (hit_time - note.time) / speed  otherwise
    and the record stores
        variance = sum(diff * diff) / count

Check: for an all-Miss run (all diffs = 0.25) the variance must be exactly 0.0625.
Observed: samples #6 and #7 are all-Miss and their f64 is exactly 0.0625. ✅

This script also shows, for every sample, the "leftover" sum of squares that the
Perfect/Good judgements must account for, and the implied std -- it should be a
plausible human timing std (tens of milliseconds), never negative.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from decode_upload import decode_token, parse_plain  # noqa: E402

UPLOADS = HERE.parents[2] / "rev" / "uploads.txt"
MISS_DIFF = 0.25
MISS_SQ = MISS_DIFF * MISS_DIFF  # 0.0625


def main() -> int:
    rows = [json.loads(l) for l in UPLOADS.read_text(encoding="utf-8").splitlines() if l.strip()]
    print(f"{'#':>2} {'chart':>6} {'P':>4} {'G':>3} {'B':>3} {'M':>5} {'n':>5} "
          f"{'variance':>20} {'sum_sq':>12} {'miss_sq':>10} {'leftover':>10} {'std_rest(ms)':>12}")
    ok = True
    for i, rec in enumerate(rows, 1):
        p = parse_plain(decode_token(rec["token"])["plain"])
        P, G, B, M = p["counts"]
        n = p["num_of_notes"]
        var = p["unknown_f64"]
        total = var * n
        miss_sq = M * MISS_SQ
        rest_cnt = P + G + B
        leftover = total - miss_sq
        std_ms = (leftover / rest_cnt) ** 0.5 * 1000 if rest_cnt else float("nan")
        flag = ""
        if leftover < 0:
            flag = "  <-- NEGATIVE, model wrong"
            ok = False
        print(f"{i:>2} {p['chart_id']:>6} {P:>4} {G:>3} {B:>3} {M:>5} {n:>5} "
              f"{var:>20.12f} {total:>12.6f} {miss_sq:>10.4f} {leftover:>10.6f} {std_ms:>12.2f}{flag}")
    print()
    print("all-Miss samples must show variance == 0.0625 exactly:")
    for i, rec in enumerate(rows, 1):
        p = parse_plain(decode_token(rec["token"])["plain"])
        if p["counts"][3] == p["num_of_notes"]:
            print(f"  #{i}: variance={p['unknown_f64']!r}  == 0.0625 -> {p['unknown_f64'] == 0.0625}")
    print(f"\nmodel consistent (no negative leftover): {ok}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
