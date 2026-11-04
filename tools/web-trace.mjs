#!/usr/bin/env node
//
// Record and summarise a Chrome performance trace of the web build, headless.
//
// The wasm build is single-threaded: everything the native build pushes to
// the compute pool (chunk meshing, EXR/probe decoding, OPFS reads, probe
// volume rebuilds) runs on the main thread between frames, so web stutter is
// "which main-thread task was long", not "which system is slow on average".
// This drives a headless Chrome over the DevTools protocol, records the
// `devtools.timeline` + `blink.user_timing` categories, and reports the
// longest main-thread tasks together with the bevy spans inside them. Spans
// exist only in a `web-profile` build (`bevy/trace` → tracing-wasm →
// `performance.measure`):
//
//   wasm-pack build --target web --release --features opfs,web-profile
//   python3 tools/serve.py 8000
//   node tools/web-trace.mjs --url 'http://localhost:8000/index.html?world=/worlds/cafe-bundle.zip&play' \
//        --out /tmp/run.jsonl  wait:20 shot:/tmp/menu.png click:640,400 wait:15 \
//        focus key:F1 trace:start hold:KeyW:10 hold:KeyA:10 trace:stop
//   node tools/web-trace.mjs --analyse /tmp/run.jsonl
//
// Steps run in order:  wait:<s>  shot:<png>  click:<x>,<y>  focus  key:<code>
// hold:<code>[,<code>]:<s>  trace:start  trace:stop.  Console errors from
// the page are echoed. Requires node 22+ and Google Chrome (`CHROME_PATH`).

import { spawn } from "node:child_process";
import { writeFileSync, rmSync } from "node:fs";

const CHROME =
  process.env.CHROME_PATH ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
// Per-process by default: two harnesses on one port attach to the same
// Chrome and trace (and drive) each other's page.
const PORT = Number(process.env.WEB_TRACE_PORT ?? 9400 + (process.pid % 500));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ── Argument parsing ────────────────────────────────────────────────────────
const argv = process.argv.slice(2);
const opt = (name) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : undefined;
};
if (opt("--analyse")) {
  await analyse(opt("--analyse"), Number(opt("--top") ?? 12));
  process.exit(0);
}
const url = opt("--url");
const out = opt("--out") ?? "/tmp/web-trace.json";
const steps = argv.filter((a, i) => !a.startsWith("--") && argv[i - 1] !== "--url" && argv[i - 1] !== "--out");
if (!url || steps.length === 0) {
  console.error("usage: web-trace.mjs --url <url> [--out trace.json] <steps…>   |   --analyse trace.json [--top N]");
  process.exit(2);
}

// ── Key codes for Input.dispatchKeyEvent ────────────────────────────────────
const KEYS = {
  KeyW: { key: "w", vk: 87 }, KeyA: { key: "a", vk: 65 }, KeyS: { key: "s", vk: 83 }, KeyD: { key: "d", vk: 68 },
  KeyQ: { key: "q", vk: 81 }, KeyE: { key: "e", vk: 69 }, Space: { key: " ", vk: 32 }, ShiftLeft: { key: "Shift", vk: 16 },
  F1: { key: "F1", vk: 112 }, Escape: { key: "Escape", vk: 27 }, Digit1: { key: "1", vk: 49 },
};

// ── Main ────────────────────────────────────────────────────────────────────
const profile = `/tmp/web-trace-profile-${process.pid}`;
const chrome = spawn(
  CHROME,
  ["--headless=new", `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check",
   "--enable-unsafe-webgpu", "--ignore-gpu-blocklist", "--window-size=1280,800",
   `--remote-debugging-port=${PORT}`, "about:blank"],
  { stdio: "ignore" },
);

const events = [];
const KEEP_NAMES = new Set(["RunTask", "FunctionCall", "DroppedFrame", "BeginFrame", "RequestAnimationFrame"]);
try {
  const ws = await connect();
  const send = rpc(ws, (method, params) => {
    if (method === "Tracing.dataCollected") {
      // Keep only what `analyse` reads: top-level tasks, frame markers and
      // bevy's spans minus the two per-system bookkeeping spans, which are
      // ~45% of the volume and never the culprit. A full trace of this
      // build is ~80k spans/s — too much to hold as one JSON string.
      for (const e of params.value) if (KEEP_NAMES.has(e.name) || (e.cat?.includes("blink.user_timing") && !/^"(check_conditions|system_commands)"/.test(e.name))) events.push(e);
    }
    else if (method === "Runtime.consoleAPICalled" && /error|warn/.test(params.type)) {
      console.log(`[page.${params.type}] ${params.args.map((a) => a.value ?? a.description ?? "").join(" ").slice(0, 300)}`);
    } else if (method === "Runtime.exceptionThrown") {
      console.log(`[page.exception] ${params.exceptionDetails.text}`);
    }
  });
  await send("Runtime.enable");
  await send("Page.enable");
  await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 800, deviceScaleFactor: 1, mobile: false });
  await send("Page.navigate", { url });
  console.log(`navigated to ${url}`);

  let traceDone;
  for (const step of steps) {
    const [kind, ...rest] = step.split(":");
    const arg = rest.join(":");
    if (kind === "wait") {
      await sleep(Number(arg) * 1000);
    } else if (kind === "shot") {
      const { result } = await send("Page.captureScreenshot", { format: "png" });
      writeFileSync(arg, Buffer.from(result.data, "base64"));
      console.log(`screenshot → ${arg}`);
    } else if (kind === "click") {
      const [x, y] = arg.split(",").map(Number);
      await send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
      await sleep(100);
      await send("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
      await sleep(80);
      await send("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1 });
      console.log(`click ${x},${y}`);
    } else if (kind === "focus") {
      // winit listens for keys on the canvas; a synthetic click does not
      // reliably focus it, so focus it directly before sending keys.
      await send("Runtime.evaluate", { expression: "document.querySelector('canvas')?.focus(); document.activeElement?.tagName" });
      console.log("focused canvas");
    } else if (kind === "key") {
      await keyDown(send, arg); await sleep(80); await keyUp(send, arg);
      console.log(`key ${arg}`);
    } else if (kind === "hold") {
      const [codes, secs] = [arg.split(":")[0].split(","), Number(arg.split(":")[1])];
      for (const c of codes) await keyDown(send, c);
      await sleep(secs * 1000);
      for (const c of codes) await keyUp(send, c);
      console.log(`held ${codes.join("+")} for ${secs}s`);
    } else if (kind === "trace" && arg === "start") {
      traceDone = new Promise((resolve) => (ws.onTracingComplete = resolve));
      await send("Tracing.start", {
        transferMode: "ReportEvents",
        traceConfig: {
          recordMode: "recordAsMuchAsPossible", traceBufferSizeInKb: 1500000,
          includedCategories: ["toplevel", "devtools.timeline", "disabled-by-default-devtools.timeline", "blink.user_timing", "disabled-by-default-devtools.timeline.frame"],
        },
      });
      console.log("tracing…");
    } else if (kind === "trace" && arg === "stop") {
      await send("Tracing.end");
      await traceDone;
      // JSON Lines, one trace event per line: a 30 s run is millions of
      // events, past what V8 will hold as a single string either way.
      const fs = await import("node:fs");
      const fd = fs.openSync(out, "w");
      for (let i = 0; i < events.length; i += 20000) {
        fs.writeSync(fd, events.slice(i, i + 20000).map((e) => JSON.stringify(e)).join("\n") + "\n");
      }
      fs.closeSync(fd);
      console.log(`trace → ${out} (${events.length} events)`);
    } else {
      console.error(`unknown step ${step}`);
    }
  }
} finally {
  chrome.kill();
  await sleep(300);
  try { rmSync(profile, { recursive: true, force: true }); } catch {}
}
if (events.length) analyseEvents(events, out, 12);

async function keyDown(send, code) {
  const k = KEYS[code] ?? { key: code, vk: 0 };
  await send("Input.dispatchKeyEvent", { type: "keyDown", key: k.key, code, windowsVirtualKeyCode: k.vk, nativeVirtualKeyCode: k.vk });
}
async function keyUp(send, code) {
  const k = KEYS[code] ?? { key: code, vk: 0 };
  await send("Input.dispatchKeyEvent", { type: "keyUp", key: k.key, code, windowsVirtualKeyCode: k.vk, nativeVirtualKeyCode: k.vk });
}

// ── CDP plumbing ────────────────────────────────────────────────────────────
async function connect() {
  let targets;
  for (let i = 0; i < 60 && !targets; i++) {
    try { targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json(); } catch { await sleep(250); }
  }
  if (!targets) throw new Error(`Chrome never listened on ${PORT}`);
  const page = targets.find((t) => t.type === "page");
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
  return ws;
}

function rpc(ws, onEvent) {
  let id = 0;
  const pending = new Map();
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id) { pending.get(msg.id)?.(msg); pending.delete(msg.id); return; }
    if (msg.method === "Tracing.tracingComplete") ws.onTracingComplete?.();
    else onEvent(msg.method, msg.params);
  };
  return (method, params = {}) => new Promise((resolve, reject) => {
    const i = ++id;
    pending.set(i, (msg) => (msg.error ? reject(new Error(`${method}: ${msg.error.message}`)) : resolve(msg)));
    ws.send(JSON.stringify({ id: i, method, params }));
  });
}

// ── Analysis ────────────────────────────────────────────────────────────────
async function analyse(path, top) {
  const { createReadStream } = await import("node:fs");
  const { createInterface } = await import("node:readline");
  const traceEvents = [];
  for await (const line of createInterface({ input: createReadStream(path), crlfDelay: Infinity })) {
    if (line) traceEvents.push(JSON.parse(line));
  }
  analyseEvents(traceEvents, path, top);
}

function analyseEvents(traceEvents, path, top) {
  // Top-level main-thread work: `RunTask` (toplevel) when captured, else the
  // rAF `FunctionCall`s bevy's frame runs in.
  let tasks = traceEvents.filter((e) => e.name === "RunTask" && e.ph === "X" && e.dur != null && e.args?.data?.frame !== undefined || (e.name === "RunTask" && e.ph === "X" && e.dur != null));
  if (!tasks.length) tasks = traceEvents.filter((e) => e.name === "FunctionCall" && e.ph === "X" && e.dur != null);
  // performance.measure → nestable async begin/end pairs on blink.user_timing.
  const open = new Map();
  const spans = [];
  for (const e of traceEvents) {
    if (!e.cat?.includes("blink.user_timing")) continue;
    const key = `${e.id ?? e.id2?.local ?? ""}|${e.name}`;
    if (e.ph === "b") open.set(key, e.ts);
    else if (e.ph === "e" && open.has(key)) {
      spans.push({ name: e.name, ts: open.get(key), dur: e.ts - open.get(key) });
      open.delete(key);
    } else if (e.ph === "X") spans.push({ name: e.name, ts: e.ts, dur: e.dur });
  }
  const ms = (us) => (us / 1000).toFixed(1).padStart(7);
  const t0 = Math.min(...tasks.map((t) => t.ts));
  const total = tasks.length ? (Math.max(...tasks.map((t) => t.ts + t.dur)) - t0) / 1000 : 0;
  const long = (lim) => tasks.filter((t) => t.dur > lim * 1000).length;
  console.log(`\n${path}: ${tasks.length} main-thread tasks over ${(total / 1000).toFixed(1)} s, ${spans.length} spans`);
  console.log(`tasks > 16 ms: ${long(16)}   > 33 ms: ${long(33)}   > 50 ms: ${long(50)}   > 100 ms: ${long(100)}`);
  if (!spans.length) console.log("(no spans — is this a web-profile build?)");

  // Where does the time go, by span name, counting only spans with no
  // enclosing span of the same name (self-time is hard without nesting info;
  // inclusive time per name is enough to rank culprits).
  const byName = new Map();
  for (const s of spans) {
    const n = shortName(s.name);
    const b = byName.get(n) ?? { total: 0, count: 0, max: 0 };
    b.total += s.dur; b.count++; b.max = Math.max(b.max, s.dur);
    byName.set(n, b);
  }
  console.log(`\nSpans by inclusive time (top ${top}):`);
  console.log(`  ${"total ms".padStart(9)} ${"max ms".padStart(8)} ${"count".padStart(6)}  span`);
  for (const [n, b] of [...byName].sort((a, b) => b[1].total - a[1].total).slice(0, top)) {
    console.log(`  ${ms(b.total).padStart(9)} ${ms(b.max).padStart(8)} ${String(b.count).padStart(6)}  ${n}`);
  }

  console.log(`\nSystems by worst single run (top ${top}):`);
  console.log(`  ${"max ms".padStart(8)} ${"total ms".padStart(9)} ${"count".padStart(6)}  system`);
  for (const [n, b] of [...byName].filter(([n]) => n.startsWith("system ")).sort((a, b) => b[1].max - a[1].max).slice(0, top)) {
    console.log(`  ${ms(b.max).padStart(8)} ${ms(b.total).padStart(9)} ${String(b.count).padStart(6)}  ${n}`);
  }

  console.log(`\nLongest main-thread tasks (top ${top}), with the longest spans inside each:`);
  for (const t of [...tasks].sort((a, b) => b.dur - a.dur).slice(0, top)) {
    console.log(`\n  task at +${((t.ts - t0) / 1e6).toFixed(2)} s: ${ms(t.dur)} ms`);
    const inside = spans
      .filter((s) => s.ts >= t.ts - 50 && s.ts + s.dur <= t.ts + t.dur + 50)
      .sort((a, b) => b.dur - a.dur)
      .slice(0, 6);
    for (const s of inside) console.log(`      ${ms(s.dur)} ms  ${shortName(s.name)}`);
  }
}

/// `"system" bevy_ecs::system name="game_engine_lib::game::world::probe_volume::update_probe_volume"`
/// → `system update_probe_volume`.
function shortName(name) {
  const m = name.match(/^"([^"]+)"\s*(\S+)?\s*(.*)$/);
  if (!m) return name;
  const [, span, , fields] = m;
  const f = fields.match(/name\s*=\s*"?([^";]+)/)?.[1]?.trim();
  if (!f) return span;
  // `avian3d::…::solve_xpbd_joint<avian3d::…::RevoluteJoint>` → keep the last
  // two path segments of the function and of any generic argument.
  const tidy = (p) => p.split("::").slice(-2).join("::");
  const g = f.match(/^([^<]+)<(.+)>$/);
  return `${span} ${g ? `${tidy(g[1])}<${tidy(g[2])}>` : tidy(f)}`;
}
