#!/usr/bin/env python3
"""Per-pass GPU timing table from an Instruments "Metal System Trace".

Usage:
    python3 tools/gpu-trace.py gpu.trace [--process game_engine] [--window A:B]

Record one with:
    xcrun xctrace record --template 'Metal System Trace' --attach game_engine
(keep the game window *in front*: a covered window stops presenting, and only
the presenting part of the trace is analysed — everything else is the app
idling at low GPU clocks and would dilute every number).

What it prints, for the presenting window:
  - frame count / period, engine and WindowServer GPU busy (union of their
    intervals — vertex/fragment overlap on Apple GPUs, so the per-pass sums
    exceed wall busy),
  - a per-pass table of fragment/vertex/compute ms per frame. wgpu makes one
    Metal command buffer per bevy render-graph pass, so the command buffer's
    label *is* the pass name — the table joins `metal-gpu-intervals` to
    `metal-application-command-buffer-submissions` on `cmdbuffer-id`.
    (`metal-object-label` is NOT used: encoder labels only cover the start of
    a trace and object ids are recycled.)
  - GPU performance-state occupancy (Minimum/Medium = the GPU idling or the
    window covered; a busy game sits at Maximum).

The xctrace XML exports are slow (~10 s each) and cached next to $TMPDIR,
keyed on the trace's path and mtime.
"""

import argparse
import hashlib
import os
import re
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
from collections import Counter, defaultdict
from pathlib import Path

TABLES = [
    "metal-gpu-intervals",
    "metal-application-command-buffer-submissions",
    "ca-client-present-request",
    "gpu-performance-state-intervals",
]


def export_tables(trace: Path, run: int) -> dict[str, Path]:
    key = hashlib.sha1(f"{trace.resolve()}:{trace.stat().st_mtime}".encode()).hexdigest()[:12]
    cache = Path(tempfile.gettempdir()) / f"gpu-trace-{key}"
    cache.mkdir(exist_ok=True)
    out = {}
    for table in TABLES:
        path = cache / f"{table}.xml"
        if not path.exists() or path.stat().st_size == 0:
            print(f"exporting {table}…", file=sys.stderr)
            xpath = f'/trace-toc/run[@number="{run}"]/data/table[@schema="{table}"]'
            with open(path, "wb") as f:
                r = subprocess.run(
                    ["xcrun", "xctrace", "export", "--input", str(trace), "--xpath", xpath],
                    stdout=f,
                    stderr=subprocess.PIPE,
                )
            if r.returncode != 0:
                path.unlink(missing_ok=True)
                sys.exit(f"xctrace export of {table} failed:\n{r.stderr.decode()}")
        out[table] = path
    return out


def rows(path: Path):
    """Rows as {column mnemonic: (text, fmt)}. xctrace deduplicates values
    with id=/ref= attributes, resolved here; absent cells are None."""
    root = ET.parse(path).getroot()
    node = root.find(".//node")
    if node is None:
        return
    cols = [c.findtext("mnemonic") for c in node.find("schema").findall("col")]
    ids = {}

    def register(e):
        if "id" in e.attrib:
            ids[e.attrib["id"]] = e
        for child in e:
            register(child)

    def text(e):
        e = ids.get(e.attrib.get("ref"), e)
        t = "".join(e.itertext()) if len(e) else (e.text or "")
        return t, e.attrib.get("fmt", t)

    for r in node.findall("row"):
        register(r)
        yield {
            name: (None if e.tag == "sentinel" else text(e))
            for name, e in zip(cols, list(r))
        }


def pass_name(label: str) -> str:
    """Command-buffer submission label → pass name. Labels look like
    'scene_pass" with 1 encoders' or 'Committed "(wgpu internal) X" without
    encoders' depending on how Instruments formatted them."""
    m = re.match(r'(?:Committed ")?(.+?)"? with(?:out)? ', label)
    name = m.group(1).strip('"') if m else label
    return name or "<unlabelled>"


def union_ms(intervals) -> float:
    """Wall-clock union of possibly-overlapping (start, end) ns intervals."""
    total, cur_s, cur_e = 0, None, None
    for s, e in sorted(intervals):
        if cur_e is None or s > cur_e:
            if cur_e is not None:
                total += cur_e - cur_s
            cur_s, cur_e = s, e
        else:
            cur_e = max(cur_e, e)
    if cur_e is not None:
        total += cur_e - cur_s
    return total / 1e6


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("trace", type=Path)
    ap.add_argument("--process", default="game_engine", help="substring of the process name")
    ap.add_argument("--window", help="override the analysed window, seconds, e.g. 1.0:2.3")
    ap.add_argument("--run", type=int, default=1, help="run number inside the trace")
    args = ap.parse_args()
    tables = export_tables(args.trace, args.run)

    def is_target(proc_cell):
        return proc_cell is not None and args.process in proc_cell[1]

    # ── The window: where the app was actually presenting ────────────────────
    presents = [
        int(r["timestamp"][0])
        for r in rows(tables["ca-client-present-request"])
        if is_target(r["process"])
    ]
    trace_end = 0
    if args.window:
        a, b = (float(x) * 1e9 for x in args.window.split(":"))
    elif len(presents) >= 10:
        a, b = min(presents), max(presents)
    else:
        print("warning: no present requests found; analysing the whole trace", file=sys.stderr)
        a, b = 0, float("inf")

    # ── GPU intervals, split engine / WindowServer / rest ────────────────────
    cb_label = {}
    for r in rows(tables["metal-application-command-buffer-submissions"]):
        if is_target(r["process"]) and r["cmdbuffer-id"]:
            cb_label[r["cmdbuffer-id"][0]] = pass_name(r["event-label"][0])

    engine, winserver = [], []
    per_pass = defaultdict(lambda: defaultdict(float))
    frames = set()
    for r in rows(tables["metal-gpu-intervals"]):
        s = int(r["start"][0])
        trace_end = max(trace_end, s)
        if not a <= s < b or r["process"] is None:
            continue
        d = int(r["duration"][0])
        span = (s, s + d)
        if is_target(r["process"]):
            engine.append(span)
            if r["frame-number"]:
                frames.add(r["frame-number"][0])
            cb = r["cmdbuffer-id"][0] if r["cmdbuffer-id"] else None
            per_pass[cb_label.get(cb, "<unlabelled>")][r["channel-name"][0]] += d / 1e6
        elif "WindowServer" in r["process"][1]:
            winserver.append(span)

    if not engine:
        sys.exit(f"no GPU intervals for a process matching {args.process!r} in the window")
    window_ms = (b - a) / 1e6 if b != float("inf") else trace_end / 1e6
    n = max(len(frames), 1)

    # ── GPU clock states inside the window ───────────────────────────────────
    states = Counter()
    for r in rows(tables["gpu-performance-state-intervals"]):
        if a <= int(r["start"][0]) < b:
            states[r["gpu-performance-state"][0]] += int(r["duration"][0]) / 1e6

    # ── Report ───────────────────────────────────────────────────────────────
    span_pct = window_ms / (trace_end / 1e6) * 100
    print(f"# {args.trace.name} — window {a / 1e9:.2f}–{b / 1e9:.2f} s "
          f"({span_pct:.0f}% of the trace), {n} frames, {window_ms / n:.2f} ms period\n")
    if span_pct < 50:
        print("> ⚠ most of the trace was not presenting (window covered?); "
              "only the window above is analysed.\n")
    eng, ws = union_ms(engine), union_ms(winserver)
    print(f"GPU busy: {args.process} {eng / window_ms * 100:.0f}% ({eng / n:.2f} ms/frame), "
          f"WindowServer {ws / window_ms * 100:.0f}% ({ws / n:.2f} ms/frame), "
          f"union {union_ms(engine + winserver) / window_ms * 100:.0f}%")
    total = sum(states.values())
    if total:
        occ = ", ".join(f"{k} {v / total * 100:.0f}%" for k, v in states.most_common())
        print(f"GPU clock: {occ}")

    print(f"\n| pass | fragment | vertex | compute |")
    print(f"|---|---|---|---|")
    key = lambda kv: -(kv[1]["Fragment"] + kv[1]["Vertex"] + kv[1]["Compute"])
    for label, ch in sorted(per_pass.items(), key=key):
        cells = " | ".join(f"{ch[c] / n:.3f}" for c in ("Fragment", "Vertex", "Compute"))
        print(f"| {label} | {cells} |")
    print("\n(ms per frame; stages overlap, so columns don't sum to the busy time)")


if __name__ == "__main__":
    main()
