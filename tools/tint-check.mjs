#!/usr/bin/env node
//
// Validate WGSL against Chrome's Tint compiler — the WebGPU front end the
// browser build actually uses.
//
// Why this exists: naga (native wgpu) hard-codes
// `DISABLE_UNIFORMITY_REQ_FOR_FRAGMENT_STAGE = true`, so it never checks that
// `textureSample`/`textureSampleBias`/`dpdx`/`fwidth` sit in uniform control
// flow. Tint does. A shader that builds and runs natively can therefore fail
// pipeline creation in the browser with nothing but
// `[Invalid ShaderModule] … While validating fragment stage` and a black
// screen. See memory.md, "WASM parity".
//
// This is a developer tool, not a `cargo test`. It takes WGSL, and our shaders
// are WESL that only becomes WGSL inside bevy's resolver at runtime, so feed it
// either a reduced repro of the construct you are unsure about or real composed
// output captured from a run.
//
// Usage:
//   node tools/tint-check.mjs shader.wgsl [more.wgsl ...]
//   cat shader.wgsl | node tools/tint-check.mjs
//
// Exits non-zero if Tint reports any error. Requires Google Chrome and node 22+
// (for the built-in `fetch` and `WebSocket`).

import { spawn } from "node:child_process";
import { readFileSync, writeFileSync, rmSync } from "node:fs";
import { basename } from "node:path";

const CHROME =
  process.env.CHROME_PATH ??
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const PORT = Number(process.env.TINT_CHECK_PORT ?? 9333);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function readStdin() {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  return Buffer.concat(chunks).toString("utf8");
}

async function main() {
  const files = process.argv.slice(2);
  const sources = files.length
    ? files.map((f) => ({ name: basename(f), code: readFileSync(f, "utf8") }))
    : [{ name: "<stdin>", code: await readStdin() }];

  if (sources.every((s) => !s.code.trim())) {
    console.error("tint-check: no WGSL given");
    process.exit(2);
  }

  // A fresh profile dir keeps this from attaching to the user's running Chrome.
  const profile = `/tmp/tint-check-profile-${process.pid}`;
  const blankPage = `/tmp/tint-check-${process.pid}.html`;
  const chrome = spawn(
    CHROME,
    [
      "--headless=new",
      `--user-data-dir=${profile}`,
      "--no-first-run",
      "--no-default-browser-check",
      // WebGPU is off by default in headless; the real adapter works on macOS,
      // the swiftshader fallback flags do not.
      "--enable-unsafe-webgpu",
      "--ignore-gpu-blocklist",
      `--remote-debugging-port=${PORT}`,
      "about:blank",
    ],
    { stdio: "ignore" },
  );

  let exitCode = 0;
  try {
    const ws = await connect();
    const send = rpc(ws);
    await send("Runtime.enable");
    await send("Page.enable");
    // `navigator.gpu` is undefined on about:blank and on `data:` URLs — WebGPU
    // needs a real page origin, so bounce through a throwaway local file.
    writeFileSync(blankPage, "<!DOCTYPE html><title>tint-check</title>");
    await send("Page.navigate", { url: `file://${blankPage}` });
    await sleep(500);

    const res = await send("Runtime.evaluate", {
      expression: `(${browserSide.toString()})(${JSON.stringify(sources)})`,
      awaitPromise: true,
      returnByValue: true,
      timeout: 60000,
    });

    const value = res.result?.result?.value;
    if (!value) {
      console.error("tint-check: no result from browser");
      console.error(JSON.stringify(res, null, 2));
      process.exit(2);
    }
    if (value.fatal) {
      console.error(`tint-check: ${value.fatal}`);
      process.exit(2);
    }

    for (const r of value.results) {
      const errors = r.messages.filter((m) => m.type === "error");
      console.log(
        `${r.name}: ${errors.length ? `${errors.length} error(s)` : "ok"}`,
      );
      for (const m of r.messages) {
        console.log(`  [${m.type}] line ${m.lineNum}: ${m.message}`);
      }
      if (errors.length) exitCode = 1;
    }
  } finally {
    chrome.kill();
    // Best-effort: Chrome is still flushing its profile as we exit, so a
    // failure here must not mask the Tint result.
    try {
      rmSync(blankPage, { force: true });
      rmSync(profile, { recursive: true, force: true });
    } catch {}
  }
  process.exit(exitCode);
}

/** Poll the DevTools endpoint until Chrome is listening, then open the page socket. */
async function connect() {
  let targets;
  for (let i = 0; i < 60; i++) {
    try {
      targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
      break;
    } catch {
      await sleep(250);
    }
  }
  if (!targets) throw new Error(`Chrome never listened on port ${PORT}`);
  const page = targets.find((t) => t.type === "page");
  if (!page) throw new Error("no page target");
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = reject;
  });
  return ws;
}

/** Minimal DevTools JSON-RPC: returns `send(method, params) -> Promise<result>`. */
function rpc(ws) {
  let id = 0;
  const pending = new Map();
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    }
  };
  return (method, params = {}) =>
    new Promise((resolve) => {
      const i = ++id;
      pending.set(i, resolve);
      ws.send(JSON.stringify({ id: i, method, params }));
    });
}

/** Runs inside the page. Stringified above, so it must be self-contained. */
async function browserSide(sources) {
  if (!navigator.gpu) return { fatal: "navigator.gpu is undefined" };
  const adapter = await navigator.gpu.requestAdapter();
  if (!adapter) return { fatal: "no WebGPU adapter" };
  const device = await adapter.requestDevice();

  const results = [];
  for (const { name, code } of sources) {
    // Swallow the uncaptured-error noise an invalid module produces; the
    // compilation info is the thing we actually want to report.
    device.pushErrorScope("validation");
    const module = device.createShaderModule({ code });
    const info = await module.getCompilationInfo();
    await device.popErrorScope();
    results.push({
      name,
      messages: info.messages.map((m) => ({
        type: m.type,
        lineNum: m.lineNum,
        message: m.message,
      })),
    });
  }
  return { results };
}

await main();
