#!/usr/bin/env python3
"""Retain case statistics and raw provenance without committing latency arrays."""
import argparse
import hashlib
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("raw", type=Path)
parser.add_argument("--location", help="Raw artifact URL or absolute archive path")
parser.add_argument("--out", type=Path)
args = parser.parse_args()
raw = args.raw.read_bytes()
data = json.loads(raw)
rows = []
for original in data["measurements"]:
    row = dict(original)
    row["sample_count"] = len(row.pop("samples"))
    if row["unit"] == "indices":
        row["note"] = "index sequence in raw artifact; sha256=" + hashlib.sha256(row["note"].encode()).hexdigest()
    rows.append(row)
summary = {
    "schema": 2,
    "raw_sha256": hashlib.sha256(raw).hexdigest(),
    "raw_location": args.location or str(args.raw.resolve()),
    "machine": data["machine"],
    "accounting_snapshot_count": len(data.get("accounting_readings", [])),
    "measurements": rows,
}
out = args.out or args.raw.with_suffix(".summary.json")
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text(json.dumps(summary, indent=2) + "\n")
print(f"{out}: {summary['raw_sha256']}  {args.raw.name}")
