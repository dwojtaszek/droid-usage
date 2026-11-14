#!/usr/bin/env python3
"""Check that every LIST_KEYS pair exists in the newest saved snapshots.

Each pair is (Droid model id, LiteLLM key). The LiteLLM key must be a rate in the newest
price snapshot, and the Droid model id must be a model in the newest multiplier snapshot.
Both snapshots live under ~/.local/share/droid-usage/. Exits 1 when a key is missing or a
snapshot does not exist.
"""

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = Path.home() / ".local" / "share" / "droid-usage"


def list_keys() -> list[tuple[str, str]]:
    tsv = ROOT / "src" / "droid-list-keys.tsv"
    if not tsv.is_file():
        sys.exit(f"{tsv} not found")
    pairs = []
    for line in tsv.read_text(encoding="utf-8").splitlines():
        if not line:
            continue
        parts = line.split("\t")
        if len(parts) != 2:
            sys.exit(f"malformed line in {tsv}: {line!r}")
        pairs.append((parts[0], parts[1]))
    if not pairs:
        sys.exit(f"{tsv} has no pairs")
    return pairs


def newest_snapshot(folder: Path) -> dict:
    # File names are UTC timestamps, so the last name in sorted order is the newest.
    files = sorted(folder.glob("*.json"))
    if not files:
        sys.exit(f"no snapshot in {folder}: run droid-usage once online to create one")
    return json.loads(files[-1].read_text(encoding="utf-8"))


def main() -> int:
    pairs = list_keys()
    rates = newest_snapshot(DATA / "prices")["rates"]
    models = newest_snapshot(DATA / "multipliers")["models"]
    missing = []
    for droid_id, litellm_key in pairs:
        if litellm_key not in rates:
            missing.append(f"price key {litellm_key} (for {droid_id}) is not in the newest price snapshot")
        if droid_id not in models:
            missing.append(f"model {droid_id} is not in the newest multiplier snapshot")
    for line in missing:
        print(line)
    print(f"checked {len(pairs)} LIST_KEYS pairs: {len(missing)} missing")
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
