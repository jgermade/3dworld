// `OcctKernel` on wasm32, through `kernel-occt/src/remote.rs`, against the
// browser's OpenCASCADE module — from Node.
//
// Two wasm instances, two linear memories, and this file is the whole of what
// joins them: the five primitives `remote.rs` asks for, installed by
// `web/occt-bridge.js` — the page's worker installs them from the same file.
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

// The same bridge the page's worker installs, from the same file.
const { installOcct } = await import(pathToFileURL(path.join(root, 'web', 'occt-bridge.js')).href);
await installOcct();

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
