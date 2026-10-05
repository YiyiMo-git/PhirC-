#!/usr/bin/env python3
"""Server probe: upload a re-encoded record to /play/upload and read the verdict.

Purpose (docs/上传成绩逆向笔记.md section 19.6): the encoding chain is fully
reproduced byte-for-byte, but the semantics of record bytes 21..23 are unknown.
The only judge is the server, so this tool replays a *captured* upload with only
those bytes changed and reports what the server says.

⚠️  Use a burner account.  Uploads go to the official leaderboard.

Typical flow
------------
1. Log the burner in through the mitmproxy capture (see capture_addon.py) and
   play one song so a real upload lands in rev/uploads.txt.
2. Read the bearer token from that capture (capture_addon.py writes it to
   rev/auth.txt when CAPTURE_AUTH=1).
3. Run:

   # dry run: build the request, print it, send nothing
   python docs/rev/probe_upload.py --auth-file rev/auth.txt --from-uploads 7 --variant zero

   # for real
   python docs/rev/probe_upload.py --auth-file rev/auth.txt --from-uploads 7 \
       --variant zero --send

Variants
--------
   keep    identical re-upload (control: validates auth/URL/body handling)
   zero    same record with bytes 21..23 = 00 00 00
   orig    same as keep but explicit
"""
from __future__ import annotations

import argparse
import json
import ssl
import sys
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from decode_upload import decode_token, parse_plain  # noqa: E402
from encode_upload import build_record, encode_token  # noqa: E402

UPLOADS = Path(__file__).resolve().parents[3] / "rev" / "uploads.txt"
ENDPOINT = "https://phira.5wyxi.com/play/upload"


def load_sample(index: int | None, use_last: bool) -> dict:
    rows = [json.loads(l) for l in UPLOADS.read_text(encoding="utf-8").splitlines() if l.strip()]
    if use_last:
        if not rows:
            raise SystemExit("rev/uploads.txt is empty")
        return rows[-1]
    if index is None:
        raise SystemExit("need --from-uploads N or --last")
    if not (1 <= index <= len(rows)):
        raise SystemExit(f"--from-uploads {index} out of range (1..{len(rows)})")
    return rows[index - 1]


def _strip_bearer(raw: str) -> str:
    raw = raw.strip().strip('"')
    if raw.lower().startswith("authorization:"):
        raw = raw.split(":", 1)[1].strip()
    if raw.lower().startswith("bearer "):
        raw = raw[7:].strip()
    return raw


def read_auth(args) -> str:
    if args.auth:
        return _strip_bearer(args.auth)
    if not args.auth_file:
        raise SystemExit("need --auth or --auth-file")
    raw = Path(args.auth_file).read_text(encoding="utf-8").strip()
    # capture_addon.py writes auth.txt as JSON lines -> prefer the newest one
    lines = [l for l in raw.splitlines() if l.strip()]
    for line in reversed(lines):
        try:
            obj = json.loads(line)
        except Exception:  # noqa: BLE001
            continue
        val = obj.get("authorization") or obj.get("Authorization")
        if val:
            return _strip_bearer(val)
    return _strip_bearer(raw)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--auth", help="bearer access token (prefer --auth-file)")
    ap.add_argument("--auth-file", help="file containing the bearer token")
    ap.add_argument("--from-uploads", type=int, help="1-based index into rev/uploads.txt")
    ap.add_argument("--last", action="store_true", help="use the newest entry in rev/uploads.txt")
    ap.add_argument("--variant", choices=["keep", "zero", "orig"], default="zero")
    ap.add_argument("--player-id", type=int, help="override the player id inside the record")
    ap.add_argument("--counts", nargs=4, type=int, metavar=("P", "G", "B", "M"),
                    help="override counts (keep the score formula consistent!)")
    ap.add_argument("--max-combo", type=int)
    ap.add_argument("--num-notes", type=int)
    ap.add_argument("--endpoint", default=ENDPOINT)
    ap.add_argument("--level", type=int, default=0,
                    help="raw-deflate level; official client uses 0 (default), keep it to stay byte-identical")
    ap.add_argument("--send", action="store_true", help="actually POST (otherwise dry run)")
    args = ap.parse_args()

    rec = load_sample(args.from_uploads, args.last)
    dec = decode_token(rec["token"])
    p = parse_plain(dec["plain"])
    assert dec["mac_ok"], "captured sample failed HMAC; wrong sample?"

    flags = bytes(p["flags_21_23"])
    if args.variant == "zero":
        new_flags = b"\x00\x00\x00"
    else:
        new_flags = flags

    counts = tuple(args.counts) if args.counts else tuple(p["counts"])
    max_combo = args.max_combo if args.max_combo is not None else p["max_combo"]
    num_of_notes = args.num_notes if args.num_notes is not None else p["num_of_notes"]
    player_id = args.player_id if args.player_id is not None else p["player_id"]

    record = build_record(
        player_id=player_id,
        chart_id=p["chart_id"],
        variance=p["unknown_f64"],
        max_combo=max_combo,
        counts=counts,
        speed_x100=p["speed_x100"],
        chart_sha256=bytes.fromhex(p["chart_sha256"]),
        num_of_notes=num_of_notes,
        u32_17=p["unknown_u32_17"],
        flags_21_23=new_flags,
        u32_24=p["unknown_u32_24"],
        u32_88=p["unknown_u32_88"],
    )
    token = encode_token(record, args.level)
    body = {"chart": rec["chart"], "token": token, "chartUpdated": rec.get("chartUpdated")}

    # sanity: decode our own token back and show what the server will see
    mine = parse_plain(decode_token(token)["plain"])
    acc = (mine["counts"][0] + 0.65 * mine["counts"][1]) / mine["num_of_notes"]
    score = round((0.9 * acc + mine["max_combo"] / mine["num_of_notes"] * 0.1) * 1e6)

    print(f"endpoint      : {args.endpoint}")
    src = "last" if args.last else f"#{args.from_uploads}"
    print(f"source sample : {src} at={rec.get('at')} chart={rec['chart']}")
    print(f"variant       : {args.variant}   flags {flags.hex(' ')} -> {new_flags.hex(' ')}")
    print(f"player_id     : {p['player_id']} -> {player_id}")
    print(f"counts        : {p['counts']} -> {list(counts)}   mc {p['max_combo']} -> {max_combo}"
          f"   n {p['num_of_notes']} -> {num_of_notes}")
    print(f"recomputed score (open-source formula): {score}")
    print(f"token len     : {len(token)}")
    print(f"body          : {json.dumps({**body, 'token': token[:48] + '...'}, ensure_ascii=False)}")

    if not args.send:
        print("\n[dry run] add --send to actually POST")
        return 0

    auth = read_auth(args)
    req = urllib.request.Request(
        args.endpoint,
        data=json.dumps(body).encode(),
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Accept-Language": "zh-CN",
            "Authorization": f"Bearer {auth}",
            "User-Agent": "probe_upload.py",
        },
    )
    ctx = ssl.create_default_context()
    print("\nPOSTing ...")
    try:
        with urllib.request.urlopen(req, timeout=30, context=ctx) as resp:
            text = resp.read().decode("utf-8", "replace")
            print(f"HTTP {resp.status}")
            print(text)
            return 0
    except urllib.error.HTTPError as e:
        text = e.read().decode("utf-8", "replace")
        print(f"HTTP {e.code}  <-- server rejected")
        print(text)
        return 2
    except Exception as e:  # noqa: BLE001
        print(f"request failed: {type(e).__name__}: {e}")
        return 3


if __name__ == "__main__":
    raise SystemExit(main())
