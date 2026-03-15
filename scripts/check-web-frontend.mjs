/**
 * Structural validation for the GitHub Pages frontend (web/).
 *
 * Checks that every DOM selector used in app.js has a matching element in
 * index.html, and that every expected event target and UI state element
 * exists.  This catches silent breakage when HTML or JS is edited
 * independently.
 */
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const webDir = path.join(scriptDir, "..", "web");

const appJs = readFileSync(path.join(webDir, "app.js"), "utf8");
const indexHtml = readFileSync(path.join(webDir, "index.html"), "utf8");

let failed = 0;
let passed = 0;

// ── 1. Extract all #id selectors from app.js ──────────────────────────
// Matches querySelector("#foo"), getElementById("foo"), #foo in selector strings
const selectorPattern = /(?:querySelector|getElementById)\(\s*["'`]#?([\w-]+)["'`]\s*\)/g;
const ids = new Set();
for (const match of appJs.matchAll(selectorPattern)) {
  ids.add(match[1]);
}

// Also extract data-field and data-side attribute selectors
const dataAttrPattern = /\[data-([\w-]+)=["'`]([\w-]+)["'`]\]/g;
const dataAttrs = [];
for (const match of appJs.matchAll(dataAttrPattern)) {
  dataAttrs.push({ attr: `data-${match[1]}`, value: match[2] });
}

// ── 2. Check each ID exists in index.html ─────────────────────────────
for (const id of ids) {
  const pattern = new RegExp(`id=["']${id}["']`);
  if (pattern.test(indexHtml)) {
    passed++;
  } else {
    console.error(`  ✗ id="${id}" used in app.js but missing from index.html`);
    failed++;
  }
}

// ── 3. Check data-attribute selectors exist ───────────────────────────
for (const { attr, value } of dataAttrs) {
  const pattern = new RegExp(`${attr}=["']${value}["']`);
  if (pattern.test(indexHtml)) {
    passed++;
  } else {
    console.error(`  ✗ ${attr}="${value}" used in app.js but missing from index.html`);
    failed++;
  }
}

// ── 3b. Check option[value="..."] selectors ──────────────────────────
// app.js queries specific <option> elements by value (e.g. option[value="svg"])
const optionValuePattern = /option\[value=["'`]([\w-]+)["'`]\]/g;
const optionValues = new Set();
for (const match of appJs.matchAll(optionValuePattern)) {
  optionValues.add(match[1]);
}
for (const val of optionValues) {
  const pattern = new RegExp(`<option[^>]+value=["']${val}["']`);
  if (pattern.test(indexHtml)) {
    passed++;
  } else {
    console.error(`  ✗ option[value="${val}"] used in app.js but missing from index.html`);
    failed++;
  }
}

// ── 3c. Check generic attribute selectors (e.g. [data-field]) ────────
// app.js uses querySelectorAll("[data-field]") to iterate all elements
// with a data-field attribute — verify index.html actually has them.
const genericAttrPattern = /querySelectorAll\(\s*["'`]\[(data-[\w-]+)\]["'`]\s*\)/g;
const genericAttrs = new Set();
for (const match of appJs.matchAll(genericAttrPattern)) {
  genericAttrs.add(match[1]);
}
for (const attr of genericAttrs) {
  const pattern = new RegExp(`${attr}=`);
  if (pattern.test(indexHtml)) {
    passed++;
  } else {
    console.error(`  ✗ [${attr}] selector used in app.js but no elements with ${attr} in index.html`);
    failed++;
  }
}

// ── 4. Verify critical structural elements ────────────────────────────
const criticalIds = [
  "dropzone",           // drag-and-drop target
  "source-file",        // file input
  "input-preview",      // source image preview
  "output-preview",     // result image preview
  "transform-button",   // main action button
  "error-box",          // error state display
  "status-line",        // busy state display
  "download-link",      // download action
  "watermark-file",     // watermark file input
  "watermark-dropzone", // watermark drag-and-drop
  "watermark-preview",  // watermark preview image
];

for (const id of criticalIds) {
  const inHtml = new RegExp(`id=["']${id}["']`).test(indexHtml);
  const inJs = appJs.includes(id);
  if (inHtml && inJs) {
    passed++;
  } else if (!inHtml) {
    console.error(`  ✗ critical element id="${id}" missing from index.html`);
    failed++;
  } else {
    console.error(`  ✗ critical element id="${id}" present in HTML but not referenced in app.js`);
    failed++;
  }
}

// ── 5. Verify WASM import path ────────────────────────────────────────
if (appJs.includes('./pkg/truss.js')) {
  passed++;
} else {
  console.error("  ✗ WASM import path './pkg/truss.js' not found in app.js");
  failed++;
}

// ── 6. Verify HTML references app.js ──────────────────────────────────
if (indexHtml.includes('src="./app.js"')) {
  passed++;
} else {
  console.error('  ✗ index.html does not reference src="./app.js"');
  failed++;
}

// ── Summary ───────────────────────────────────────────────────────────
console.log(`\n  web/ structural check: ${passed} passed, ${failed} failed`);
if (failed > 0) {
  process.exit(1);
}
