#!/usr/bin/env node
//
// Show what the web start page would import a world zip as, without a browser.
//
// `index.html` unpacks a bundle into OPFS with its own JavaScript (see
// docs/WORLD_SHARING.md). The zip reader, bundle-layout mapping and manifest
// (de)serialisation are pure functions kept between `@bundle-logic-start` /
// `@bundle-logic-end` markers in that file; this tool evaluates that exact
// slice — nothing is duplicated — and prints the resulting import plan:
// OPFS keys, world ids, project manifests. It also decompresses every entry
// so a bad archive fails here rather than in the page.
//
// Usage:
//   node tools/bundle-check.mjs worlds/cafe.zip [--keys]
//
// Exits non-zero if the archive would be refused. Requires node 22+.

import { readFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const args = process.argv.slice(2);
const showKeys = args.includes("--keys");
const zipPath = args.find((a) => !a.startsWith("--"));
if (!zipPath) {
    console.error("usage: node tools/bundle-check.mjs <bundle.zip> [--keys]");
    process.exit(2);
}

const here = dirname(fileURLToPath(import.meta.url));
const html = readFileSync(join(here, "..", "index.html"), "utf8");
const start = html.indexOf("// @bundle-logic-start");
const end = html.indexOf("// @bundle-logic-end");
if (start < 0 || end < 0) {
    console.error("index.html: bundle-logic markers not found");
    process.exit(2);
}
const logic = html.slice(html.indexOf("\n", start) + 1, end);
// The slice declares plain functions/consts; wrap it so we can pull them out.
const { readZipDirectory, zipEntryData, planBundle, parseManifest, serializeManifest } =
    new Function(`${logic}\nreturn { readZipDirectory, zipEntryData, planBundle, parseManifest, serializeManifest };`)();

const bytes = new Uint8Array(readFileSync(zipPath));
const nameHint = basename(zipPath).replace(/\.zip$/i, "");

let plan;
try {
    const entries = readZipDirectory(bytes);
    plan = planBundle(entries, nameHint);
    console.log(`${zipPath}: ${entries.length} entries, ${plan.writes.length} to import` +
        (plan.ignored ? `, ${plan.ignored} ignored` : ""));
} catch (e) {
    console.error(`refused: ${e.message}`);
    process.exit(1);
}

console.log(`layout:   ${plan.synthesise ? "hand-zipped world folder (project synthesised)" : "exporter bundle"}`);
console.log(`worlds:   ${plan.worlds.join(", ")}`);

let total = 0, failures = 0;
for (const { key, entry } of plan.writes) {
    try {
        const data = await zipEntryData(bytes, entry);
        if (data.length !== entry.size) throw new Error(`size mismatch (${data.length} ≠ ${entry.size})`);
        total += data.length;
        if (showKeys) console.log(`  ${key}  ${data.length}`);
    } catch (e) {
        failures++;
        console.error(`  ${key}: ${e.message}`);
    }
}
console.log(`payload:  ${(total / 1048576).toFixed(1)} MB in ${plan.writes.length} files`);

const text = new TextDecoder();
for (const { id, entry } of plan.projects) {
    const m = parseManifest(text.decode(await zipEntryData(bytes, entry)));
    console.log(`project:  ${id} — name ${JSON.stringify(m.name)}, worlds [${m.worlds.join(", ")}], start ${m.start_world}`);
    console.log(serializeManifest(m).replace(/^/gm, "    "));
}
if (plan.synthesise) {
    for (const id of plan.worlds) {
        console.log(`project:  ${id} (synthesised)`);
        console.log(serializeManifest({ name: id, plugins: [], worlds: [id], start_world: id }).replace(/^/gm, "    "));
    }
}
if (failures) {
    console.error(`${failures} entr${failures === 1 ? "y" : "ies"} failed to decode`);
    process.exit(1);
}
