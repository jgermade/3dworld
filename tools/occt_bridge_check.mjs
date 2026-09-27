// `OcctKernel` on wasm32, through `kernel-occt/src/remote.rs`, against the
// browser's OpenCASCADE module — from Node.
//
// Two wasm instances, two linear memories, and this file is the whole of what
// joins them: the five primitives `remote.rs` asks for, on `globalThis.w3dOcct`.
// The page will install the same five; nothing here knows the header.
//
// Prints the conformance report, then the reference rows. Exits non-zero if a
// conformance check failed; the rows are compared by `tools/occt_wasm_check.py`.
//
// Usage: node tools/occt_bridge_check.mjs [conformance|reference|both]

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), '..');
const what = process.argv[2] ?? 'both';

const { default: createOcct } = await import(
  pathToFileURL(path.join(root, 'web', 'dist', 'occt', 'w3d_occt.mjs')).href
);
const M = await createOcct();

globalThis.w3dOcct = {
  call: (name, args) => {
    const fn = M[`_w3d_occt_${name}`];
    if (!fn) throw new Error(`the OpenCASCADE module exports no w3d_occt_${name}`);
    return fn(...args);
  },
  malloc: (len) => M._malloc(len),
  free: (ptr) => M._free(ptr),
  // `M.HEAPU8` is read on every call, never kept: the module's memory grows,
  // and growing it replaces the buffer every earlier view was made of.
  write: (ptr, bytes) => M.HEAPU8.set(bytes, ptr),
  read: (ptr, out) => out.set(M.HEAPU8.subarray(ptr, ptr + out.length)),
};

const bridgeDir = path.join(root, 'build', 'occt-bridge');
const bridge = await import(pathToFileURL(path.join(bridgeDir, 'w3d_occt_bridge_check.js')).href);
bridge.initSync({ module: fs.readFileSync(path.join(bridgeDir, 'w3d_occt_bridge_check_bg.wasm')) });

let failed = 0;
if (what === 'conformance' || what === 'both') {
  const started = performance.now();
  const report = bridge.conformance();
  const lines = report.split('\n');
  failed = lines.filter((l) => l.startsWith('FAILED')).length;
  if (what === 'both') console.log('# conformance');
  for (const l of lines) console.log(l);
  console.error(
    `conformance through the bridge: ${lines.length - failed} of ${lines.length} checks passed ` +
      `in ${(performance.now() - started).toFixed(0)} ms`,
  );
}
if (what === 'reference' || what === 'both') {
  if (what === 'both') console.log('# reference');
  console.log(bridge.reference());
}
process.exit(failed ? 1 : 0);
