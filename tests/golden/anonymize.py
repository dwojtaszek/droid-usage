#!/usr/bin/env python3
"""Builds the anonymized golden fixtures under tests/golden/home from real session files.

Run from the repository root, with the real home folder:

    python3 tests/golden/anonymize.py --home ~ --out tests/golden/home

The fixtures keep what the readers and the report use: token counts, model
names, timestamps, recorded costs, and which sessions share a project. They drop
message text, tool output, session and conversation IDs, and real paths. Each
real project folder becomes `/work/project-NN`.

The script checks that every reduced file gives the same reader result as its
original (project, start time, model, and token sums). It then prints a summary
to compare with the golden report by hand.

To regenerate the expected output after a fixture change:

    cargo build -q
    HOME=$PWD/tests/golden/home NO_COLOR=1 target/debug/droid-usage --offline \\
        > tests/golden/report.txt 2> tests/golden/warnings.txt

Run the built binary, not `cargo run`. With HOME set to the fixtures, rustup cannot find its toolchain, so `cargo run` fails.

The script removes `--out` before writing it. Point it only at tests/golden/home.
"""

import argparse
import glob
import json
import os
import re
import shutil
import sqlite3
from datetime import datetime

DROID_SAMPLE = 10
AGY_WITH_TOKENS = 6
AGY_WITHOUT_TOKENS = 2
CODEX_SAMPLE = 8
PI_SAMPLE = 6
DROID_TOKEN_KEYS = ["inputTokens", "outputTokens", "cacheReadTokens", "cacheCreationTokens"]
PI_TOKEN_KEYS = ["input", "output", "cacheRead", "cacheWrite"]
OPENCODE_COLUMNS = [
    "directory",
    "model",
    "time_created",
    "tokens_input",
    "tokens_output",
    "tokens_cache_read",
    "tokens_cache_write",
    "cost",
]
AGY_TOKEN_KEYS = ["input_tokens", "output_tokens", "cache_read_tokens"]
AGY_MODEL_PREFIXES = (b"gemini-", b"claude-", b"gpt-")


def stride(items, count):
    """Evenly spaced items, so a sample covers the whole sorted list."""
    if len(items) <= count:
        return list(items)
    return [items[i * len(items) // count] for i in range(count)]


def load_json(path):
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)
    except (OSError, ValueError):
        return None


def write_json(path, value):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=1)
        handle.write("\n")


def parse_time(text):
    try:
        return datetime.fromisoformat(text.replace("Z", "+00:00"))
    except (AttributeError, ValueError):
        return None


def project_path(real, projects):
    """Maps a real project folder to a synthetic one, the same way every time."""
    if real is None:
        return None
    if real not in projects:
        projects[real] = f"/work/project-{len(projects) + 1:02d}"
    return projects[real]


def droid_header(jsonl):
    """Project and first message time, read the same way as the Droid reader."""
    project, started, role = None, None, None
    with open(jsonl, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                value = json.loads(line)
            except ValueError:
                continue
            if not isinstance(value, dict):
                continue
            if value.get("type") == "session_start" and project is None:
                project = value.get("cwd")
            if value.get("type") == "message" and isinstance(value.get("timestamp"), str):
                started = value["timestamp"]
                role = (value.get("message") or {}).get("role")
                break
    return project, started, role


def droid(home, out, projects, summary):
    root = os.path.join(home, ".factory/sessions")
    settings = sorted(glob.glob(os.path.join(root, "**", "*.settings.json"), recursive=True))
    usable, without_usage, broken = [], [], []
    for path in settings:
        data = load_json(path)
        if not isinstance(data, dict):
            broken.append(path)
        elif isinstance(data.get("tokenUsage"), dict):
            usable.append(path)
        else:
            without_usage.append(path)

    models = set()
    unreadable = len(broken)
    out_root = os.path.join(out, ".factory/sessions")
    for index, path in enumerate(stride(usable, DROID_SAMPLE) + without_usage[:2], 1):
        data = load_json(path)
        jsonl = path[: -len(".settings.json")] + ".jsonl"
        real_project, started, role = (
            droid_header(jsonl) if os.path.exists(jsonl) else (None, None, None)
        )
        # The reader skips a session with no transcript or no timed message.
        if started is None:
            unreadable += 1
        project = project_path(real_project, projects)
        folder = project.strip("/").replace("/", "-") if project else "no-project"

        usage = data.get("tokenUsage")
        settings_out = {}
        if isinstance(data.get("model"), str):
            settings_out["model"] = data["model"]
            models.add(data["model"])
        if usage is None or isinstance(usage, dict):
            settings_out["tokenUsage"] = (
                None
                if usage is None
                else {k: v for k, v in usage.items() if isinstance(v, int)}
            )
        name = f"droid-{index:02d}"
        write_json(os.path.join(out_root, folder, f"{name}.settings.json"), settings_out)

        if os.path.exists(jsonl):
            lines = []
            if project is not None:
                lines.append({"type": "session_start", "cwd": project})
            if started is not None:
                message = {"type": "message", "timestamp": started}
                if role is not None:
                    message["message"] = {"role": role}
                lines.append(message)
            reduced = os.path.join(out_root, folder, f"{name}.jsonl")
            write_lines(reduced, lines)
            if droid_header(reduced) != (project, started, role):
                raise SystemExit(f"reduced header differs for {path}")

        if started is not None:
            tokens = sum(
                (usage or {}).get(key, 0) for key in DROID_TOKEN_KEYS if isinstance(usage, dict)
            )
            summary.add("droid", model=data.get("model"), tokens=tokens)

    write_text(os.path.join(out_root, "no-project", "droid-broken.settings.json"), "{not json")
    summary.skipped["droid"] = unreadable
    return models


def write_lines(path, lines):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        for line in lines:
            handle.write(json.dumps(line) + "\n")


def write_text(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(text)


def opencode(home, out, projects, summary):
    source = os.path.join(home, ".local/share/opencode/opencode.db")
    conn = sqlite3.connect(f"file:{source}?mode=ro", uri=True)
    declared = {row[1]: row[2] for row in conn.execute("pragma table_info(session_v2)")}
    rows = conn.execute(f"select {', '.join(OPENCODE_COLUMNS)} from session_v2").fetchall()
    conn.close()

    target = os.path.join(out, ".local/share/opencode/opencode.db")
    os.makedirs(os.path.dirname(target), exist_ok=True)
    out_conn = sqlite3.connect(target)
    columns = ", ".join(f"{name} {declared[name]}" for name in OPENCODE_COLUMNS)
    out_conn.execute(f"CREATE TABLE session_v2 ({columns})")
    models = set()
    for row in rows:
        directory, model, created, t_in, t_out, t_read, t_write, cost = row
        model_id = model
        if isinstance(model, str) and model.startswith("{"):
            model_id = (json.loads(model) or {}).get("id")
        out_model = json.dumps({"id": model_id, "providerID": "fixture"}) if model_id else None
        if model_id:
            models.add(model_id)
        out_conn.execute(
            f"INSERT INTO session_v2 VALUES ({', '.join('?' * len(OPENCODE_COLUMNS))})",
            (
                project_path(directory, projects),
                out_model,
                created,
                t_in,
                t_out,
                t_read,
                t_write,
                cost,
            ),
        )
        summary.add("opencode", model=model_id, tokens=(t_in or 0) + (t_out or 0) + (t_read or 0) + (t_write or 0))
    out_conn.commit()
    out_conn.close()
    summary.skipped["opencode"] = 0
    return models


def read_transcript(path):
    """Every line that parses as JSON, as dicts."""
    values = []
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                value = json.loads(line)
            except ValueError:
                continue
            if isinstance(value, dict):
                values.append(value)
    return values


def transcript_totals(values):
    """Earliest time and token sums, as the agy reader sees them."""
    started = None
    totals = {key: 0 for key in AGY_TOKEN_KEYS}
    for value in values:
        at = parse_time(value.get("created_at"))
        if at is not None and (started is None or at < started):
            started = at
        for key in AGY_TOKEN_KEYS:
            totals[key] += value.get(key, 0) if isinstance(value.get(key, 0), int) else 0
    return started, totals


def reduce_transcript(values):
    """Keeps the lines the reader uses: token lines, and the earliest timed line."""
    earliest = None
    for value in values:
        at = parse_time(value.get("created_at"))
        if at is not None and (earliest is None or at < earliest[0]):
            earliest = (at, value)
    kept = []
    for value in values:
        if any(key in value for key in AGY_TOKEN_KEYS) or (earliest and value is earliest[1]):
            line = {key: value[key] for key in ("type", "status", "created_at") if key in value}
            line.update({key: value[key] for key in AGY_TOKEN_KEYS if key in value})
            kept.append(line)
    return kept


def first_model_name(blob):
    for token in re.split(rb"[^A-Za-z0-9._-]", blob):
        if token.startswith(AGY_MODEL_PREFIXES):
            return token.decode("ascii")
    return None


def conversation_model(path):
    if not os.path.exists(path):
        return None
    conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        blobs = conn.execute("SELECT data FROM gen_metadata ORDER BY idx").fetchall()
    except sqlite3.Error:
        return None
    finally:
        conn.close()
    for (blob,) in blobs:
        name = first_model_name(blob)
        if name:
            return name
    return None


def agy(home, out, summary):
    root = os.path.join(home, ".gemini/antigravity-cli")
    transcripts = sorted(
        glob.glob(os.path.join(root, "brain", "*", ".system_generated", "logs", "transcript.jsonl"))
    )
    with_tokens, without = [], []
    for path in transcripts:
        values = read_transcript(path)
        if any(any(key in value for key in AGY_TOKEN_KEYS) for value in values):
            with_tokens.append(path)
        else:
            without.append(path)

    models = set()
    for index, path in enumerate(
        stride(with_tokens, AGY_WITH_TOKENS) + without[:AGY_WITHOUT_TOKENS], 1
    ):
        real_id = os.path.basename(os.path.dirname(os.path.dirname(os.path.dirname(path))))
        values = read_transcript(path)
        started, totals = transcript_totals(values)
        reduced = reduce_transcript(values)
        check_transcript(reduced, started, totals, path)

        name = f"agy-{index:02d}"
        write_lines(
            os.path.join(out, ".gemini/antigravity-cli/brain", name, ".system_generated/logs/transcript.jsonl"),
            reduced,
        )
        model = conversation_model(os.path.join(root, "conversations", f"{real_id}.db"))
        if model:
            models.add(model)
            db = os.path.join(out, ".gemini/antigravity-cli/conversations", f"{name}.db")
            os.makedirs(os.path.dirname(db), exist_ok=True)
            blob = b"\x0a" + model.encode("ascii") + b"\x00"
            conn = sqlite3.connect(db)
            conn.execute("CREATE TABLE gen_metadata (idx INTEGER, data BLOB, size INTEGER)")
            conn.execute("INSERT INTO gen_metadata VALUES (0, ?, ?)", (blob, len(blob)))
            conn.commit()
            conn.close()
        summary.add("agy", model=model, tokens=sum(totals.values()))

    summary.skipped["agy"] = 0
    return models


def check_transcript(reduced, started, totals, path):
    got_started, got_totals = transcript_totals(reduced)
    if got_started != started or got_totals != totals:
        raise SystemExit(f"reduced transcript differs for {path}")


def nonneg_int(value):
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else 0


def parse_codex_usage(usage):
    if not isinstance(usage, dict):
        return None
    raw_in = usage.get("input_tokens")
    raw_out = usage.get("output_tokens")
    if not isinstance(raw_in, int) or isinstance(raw_in, bool) or raw_in < 0:
        return None
    if not isinstance(raw_out, int) or isinstance(raw_out, bool) or raw_out < 0:
        return None
    cache_read = nonneg_int(usage.get("cached_input_tokens"))
    cache_write = nonneg_int(usage.get("cache_write_input_tokens"))
    return {
        "input": max(0, raw_in - cache_read - cache_write),
        "output": raw_out,
        "cache_read": cache_read,
        "cache_write": cache_write,
    }


def codex_totals(values):
    started_str, started, project, model = None, None, None, None
    totals = {"input": 0, "output": 0, "cache_read": 0, "cache_write": 0}
    for event in values:
        kind = event.get("type")
        payload = event.get("payload") if isinstance(event.get("payload"), dict) else {}
        if kind == "session_meta" and started is None:
            ts = payload.get("timestamp") if isinstance(payload.get("timestamp"), str) else None
            at = parse_time(ts)
            if at is not None:
                started_str, started = ts, at
                project = payload.get("cwd") if isinstance(payload.get("cwd"), str) else None
        elif kind == "turn_context":
            if isinstance(payload.get("model"), str):
                model = payload["model"]
        elif kind == "event_msg" and payload.get("type") == "token_count":
            info = payload.get("info") if isinstance(payload.get("info"), dict) else {}
            parsed = parse_codex_usage(info.get("total_token_usage"))
            if parsed is not None:
                totals = parsed
        elif kind == "token_usage_record":
            parsed = parse_codex_usage(payload.get("thread_token_usage"))
            if parsed is not None:
                totals = parsed
    return started_str, started, project, model, totals


def codex(home, out, projects, summary):
    root = os.path.join(home, ".codex")
    files = sorted(
        path
        for sub in ("sessions", "archived_sessions")
        for path in glob.glob(os.path.join(root, sub, "**", "*.jsonl"), recursive=True)
    )
    usable, without_usage, broken = [], [], []
    for path in files:
        _, started, _, _, totals = codex_totals(read_transcript(path))
        if started is None:
            broken.append(path)
        elif sum(totals.values()) > 0:
            usable.append(path)
        else:
            without_usage.append(path)

    models = set()
    out_root = os.path.join(out, ".codex/sessions")
    for index, path in enumerate(stride(usable, CODEX_SAMPLE) + without_usage[:2], 1):
        started_str, started, real_project, model, totals = codex_totals(read_transcript(path))
        project = project_path(real_project, projects)
        meta_payload = {"timestamp": started_str}
        if project is not None:
            meta_payload["cwd"] = project
        lines = [{"type": "session_meta", "payload": meta_payload}]
        if model:
            models.add(model)
            lines.append({"type": "turn_context", "payload": {"model": model}})
        if sum(totals.values()) > 0:
            lines.append(
                {
                    "type": "event_msg",
                    "payload": {
                        "type": "token_count",
                        "info": {
                            "total_token_usage": {
                                "input_tokens": totals["input"]
                                + totals["cache_read"]
                                + totals["cache_write"],
                                "cached_input_tokens": totals["cache_read"],
                                "cache_write_input_tokens": totals["cache_write"],
                                "output_tokens": totals["output"],
                            }
                        },
                    },
                }
            )
        reduced = os.path.join(out_root, f"codex-{index:02d}.jsonl")
        write_lines(reduced, lines)
        if codex_totals(read_transcript(reduced))[1:] != (started, project, model, totals):
            raise SystemExit(f"reduced codex transcript differs for {path}")
        summary.add("codex", model=model, tokens=sum(totals.values()))

    summary.skipped["codex"] = len(broken)
    return models


def pi_totals(values):
    started_str, started, project, model = None, None, None, None
    totals = {key: 0 for key in PI_TOKEN_KEYS}
    cost = None

    def add_usage(usage):
        nonlocal cost
        if not isinstance(usage, dict):
            return
        raw_in, raw_out = usage.get("input"), usage.get("output")
        if not isinstance(raw_in, int) or isinstance(raw_in, bool) or raw_in < 0:
            return
        if not isinstance(raw_out, int) or isinstance(raw_out, bool) or raw_out < 0:
            return
        for key in PI_TOKEN_KEYS:
            totals[key] += nonneg_int(usage.get(key))
        raw_cost = (usage.get("cost") or {}).get("total") if isinstance(usage.get("cost"), dict) else None
        if isinstance(raw_cost, (int, float)) and not isinstance(raw_cost, bool) and raw_cost > 0:
            cost = (cost or 0.0) + float(raw_cost)

    for entry in values:
        kind = entry.get("type")
        if kind == "session" and started is None:
            ts = entry.get("timestamp") if isinstance(entry.get("timestamp"), str) else None
            at = parse_time(ts)
            if at is not None:
                started_str, started = ts, at
                project = entry.get("cwd") if isinstance(entry.get("cwd"), str) else None
        elif kind == "model_change":
            if isinstance(entry.get("modelId"), str):
                model = entry["modelId"]
        elif kind == "message":
            msg = entry.get("message") if isinstance(entry.get("message"), dict) else {}
            if isinstance(msg.get("model"), str):
                model = msg["model"]
            add_usage(msg.get("usage"))
        elif kind in ("usage", "compaction", "branch_summary"):
            if isinstance(entry.get("model"), str):
                model = entry["model"]
            add_usage(entry.get("usage"))
    return started_str, started, project, model, totals, cost


def pi(home, out, projects, summary):
    root = os.path.join(home, ".pi/agent/sessions")
    files = sorted(glob.glob(os.path.join(root, "*", "*.jsonl")))
    usable, without_usage, broken = [], [], []
    for path in files:
        _, started, _, _, totals, _ = pi_totals(read_transcript(path))
        if started is None:
            broken.append(path)
        elif sum(totals.values()) > 0:
            usable.append(path)
        else:
            without_usage.append(path)

    models = set()
    out_root = os.path.join(out, ".pi/agent/sessions")
    for index, path in enumerate(stride(usable, PI_SAMPLE) + without_usage[:2], 1):
        started_str, started, real_project, model, totals, cost = pi_totals(read_transcript(path))
        project = project_path(real_project, projects)
        folder = f"--{project.strip('/').replace('/', '-')}--" if project else "--no-project--"
        header = {"type": "session", "version": 3, "timestamp": started_str}
        if project is not None:
            header["cwd"] = project
        lines = [header]
        if model:
            models.add(model)
        if sum(totals.values()) > 0 or cost is not None:
            usage = dict(totals)
            if cost is not None:
                usage["cost"] = {"total": cost}
            entry = {"type": "usage", "usage": usage}
            if model:
                entry["model"] = model
            lines.append(entry)
        elif model:
            lines.append({"type": "model_change", "modelId": model})
        reduced = os.path.join(out_root, folder, f"pi-{index:02d}.jsonl")
        write_lines(reduced, lines)
        if pi_totals(read_transcript(reduced))[1:] != (started, project, model, totals, cost):
            raise SystemExit(f"reduced pi transcript differs for {path}")
        summary.add("pi", model=model, tokens=sum(totals.values()))

    summary.skipped["pi"] = len(broken)
    return models


def snapshots(home, out, models):
    base = os.path.join(home, ".local/share/droid-usage")
    price_files = sorted(glob.glob(os.path.join(base, "prices", "*.json")))
    multiplier_files = sorted(glob.glob(os.path.join(base, "multipliers", "*.json")))
    if not price_files or not multiplier_files:
        raise SystemExit("need one saved price and one multiplier snapshot in ~/.local/share/droid-usage")

    wanted = set()
    for model in models:
        wanted.add(model)
        for suffix in ("-ultrafast", "-fast"):
            if model.endswith(suffix):
                wanted.add(model[: -len(suffix)])
    price = load_json(price_files[-1])
    kept = {key: rate for key, rate in price["rates"].items() if key.split("/")[-1] in wanted}
    write_json(
        os.path.join(out, ".local/share/droid-usage/prices", os.path.basename(price_files[-1])),
        {"fetched": price["fetched"], "rates": kept},
    )
    multiplier = load_json(multiplier_files[-1])
    write_json(
        os.path.join(out, ".local/share/droid-usage/multipliers", os.path.basename(multiplier_files[-1])),
        multiplier,
    )
    return len(kept)


class Summary:
    def __init__(self):
        self.sources = {}
        self.skipped = {}

    def add(self, source, model, tokens):
        entry = self.sources.setdefault(source, {"sessions": 0, "with_model": 0, "tokens": 0})
        entry["sessions"] += 1
        entry["with_model"] += 1 if model else 0
        entry["tokens"] += tokens

    def show(self, kept_prices):
        for source, entry in sorted(self.sources.items()):
            print(
                f"{source}: {entry['sessions']} sessions, {entry['with_model']} with a model, "
                f"{entry['tokens']} tokens, {self.skipped.get(source, 0)} unreadable entries"
            )
        print(f"price rates kept: {kept_prices}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--home", required=True, help="real home folder to read")
    parser.add_argument("--out", required=True, help="fixture home folder to write (removed first)")
    args = parser.parse_args()
    home = os.path.expanduser(args.home)
    out = os.path.abspath(args.out)

    if os.path.isdir(out):
        shutil.rmtree(out)
    os.makedirs(out)

    projects = {}
    summary = Summary()
    models = set()
    models |= droid(home, out, projects, summary)
    models |= opencode(home, out, projects, summary)
    models |= agy(home, out, summary)
    models |= codex(home, out, projects, summary)
    models |= pi(home, out, projects, summary)
    kept = snapshots(home, out, models)
    summary.show(kept)


if __name__ == "__main__":
    main()
