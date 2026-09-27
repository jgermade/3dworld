// The wasm half of `make occt-wasm-check`: the scenes of
// `kernel-occt/examples/occt_wasm_reference.rs`, built by the browser's
// OpenCASCADE module (web/dist/occt/w3d_occt.mjs) through the C seam itself —
// `w3d_occt.h`, called from JS, the pointers into the module's own memory.
//
// Prints the same `name<TAB>value` rows as the native half, and then one more
// kind of row the native half cannot make: an OCCT exception thrown and caught
// *inside wasm*. The Rust side validates a profile before the shim sees it, so
// on the desktop that path is unreachable; here the shim is called directly
// with a polygon of one repeated point, OCCT throws `StdFail_NotDone`, and the
// answer must be an error code and OCCT's message — not a trap. It is the only
// evidence that `-fwasm-exceptions` works, and a build without it passes every
// other row here.
//
// Usage: node tools/occt_wasm_check.mjs [path/to/w3d_occt.mjs]

import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const modulePath = process.argv[2] ?? path.join(here, '..', 'web', 'dist', 'occt', 'w3d_occt.mjs');
const { default: createOcct } = await import(pathToFileURL(modulePath).href);

const startedLoad = performance.now();
const M = await createOcct();
const loadMs = performance.now() - startedLoad;

const OK = 0;
const KINDS = { 1: 'unknown-body', 2: 'degenerate', 3: 'unsupported', 4: 'failed' };

// Scratch space in the module's heap for out-parameters.
const scratch = M._malloc(256);
const u32 = (p) => M.HEAPU32[p >>> 2];
const f64 = (p, i) => M.HEAPF64[(p >>> 3) + i];

function call(name, ...args) {
  const code = M[`_w3d_occt_${name}`](...args);
  return code;
}

function lastError() {
  return M.UTF8ToString(M._w3d_occt_last_error());
}

/** Calls an entry point whose last argument is a `uint32_t *out`. */
function make(name, ...args) {
  const code = call(name, ...args, scratch);
  if (code !== OK) throw new Error(`${name}: ${KINDS[code] ?? code}: ${lastError()}`);
  return u32(scratch);
}

function tryMake(name, ...args) {
  const code = call(name, ...args, scratch);
  return code === OK ? { body: u32(scratch) } : { error: KINDS[code] ?? String(code) };
}

function doubles(values) {
  const p = M._malloc(values.length * 8);
  M.HEAPF64.set(values, p >>> 3);
  return p;
}

const ctx = M._w3d_occt_context_new();

function topo(body) {
  const code = call('topology', ctx, body, scratch);
  if (code !== OK) throw new Error(`topology: ${lastError()}`);
  return [0, 1, 2, 3].map((i) => u32(scratch + i * 4)).join(' ');
}

// `W3dOcctMesh` on wasm32: seven pointers, four u32 counts, one owner pointer.
function mesh(body) {
  const out = M._malloc(48);
  const code = call('tessellate', ctx, body, 0.01, 0.35, out);
  if (code !== OK) throw new Error(`tessellate: ${lastError()}`);
  const vertices = u32(out + 28);
  const triangles = u32(out + 32);
  const lineSegments = u32(out + 40);
  M._w3d_occt_mesh_free(out);
  M._free(out);
  return `${triangles} ${vertices} ${lineSegments}`;
}

function bounds(body) {
  const code = call('bounds', ctx, body, scratch);
  if (code !== OK) throw new Error(`bounds: ${lastError()}`);
  return [0, 1, 2, 3, 4, 5].map((i) => f64(scratch, i).toFixed(9)).join(' ');
}

const rows = [];
const row = (name, value) => rows.push([name, value]);
const started = performance.now();

const cube = make('make_box', ctx, 20, 20, 20);
row('cube.topology', topo(cube));

const plate = make('make_box', ctx, 40, 40, 10);
const drill0 = make('make_cylinder', ctx, 6, 20);
const m34 = doubles([1, 0, 0, 8, 0, 1, 0, 0, 0, 0, 1, 0]);
const drill = make('transform', ctx, drill0, m34);
const cut = make('boolean', ctx, 1, plate, drill, 1e-7);
row('cut.topology', topo(cut));
row('cut.mesh', mesh(cut));
row('cut.bounds', bounds(cut));

const fillet = make('fillet', ctx, cube, 2);
row('fillet.topology', topo(fillet));
row('fillet.mesh', mesh(fillet));
row('chamfer.topology', topo(make('chamfer', ctx, cube, 2)));
row('shell.topology', topo(make('shell', ctx, cube, 0, 1)));

for (const r of [10, 15]) {
  const res = tryMake('fillet', ctx, cube, r);
  row(`fillet.r${r}`, res.error ?? topo(res.body));
}

// `W3dOcctBytes`: data pointer, length, owner.
const bytes = M._malloc(12);
if (call('save_body', ctx, cut, bytes) !== OK) throw new Error(`save_body: ${lastError()}`);
const back = make('load_body', ctx, u32(bytes), u32(bytes + 4));
M._w3d_occt_bytes_free(bytes);
row('brep.topology', topo(back));

const ids = M._malloc(4);
M.HEAPU32[ids >>> 2] = cut;
if (call('export_step', ctx, ids, 1, bytes) !== OK) throw new Error(`export_step: ${lastError()}`);
// `W3dOcctImport` on wasm32: seven pointer-or-u32 fields and an owner.
const imported = M._malloc(32);
const code = call('import_step', ctx, u32(bytes), u32(bytes + 4), imported);
if (code !== OK) throw new Error(`import_step: ${lastError()}`);
M._w3d_occt_bytes_free(bytes);
const count = u32(imported + 12);
row('step.bodies', String(count));
row('step.topology', topo(u32(u32(imported))));
M._w3d_occt_import_free(imported);

const elapsedMs = performance.now() - started;
for (const [n, v] of rows) console.log(`${n}\t${v}`);

// --- wasm only -------------------------------------------------------------

// A polygon whose three corners are one point, on the XY plane:
// `W3dOcctProfile` is kind, p1, p2, vertices, count, then origin, x axis and
// y axis, 8-byte aligned on wasm32. `BRepBuilderAPI_MakePolygon` drops the
// repeated points, and `Wire()` on what is left throws `StdFail_NotDone` —
// unconditionally, which matters: OCCT's release build defines `No_Exception`
// and compiles its `Raise_if` checks out, so most of its "exceptions" never
// throw in either build. This one does in both. The Rust side refuses such a
// profile before the shim sees it (`Profile::validate`), so only a direct call
// reaches the throw.
const corners = doubles([1, 1, 1, 1, 1, 1]);
const profile = M._malloc(104);
M.HEAP32[profile >>> 2] = 2;
M.HEAPF64[(profile + 8) >>> 3] = 0;
M.HEAPF64[(profile + 16) >>> 3] = 0;
M.HEAPU32[(profile + 24) >>> 2] = corners;
M.HEAPU32[(profile + 28) >>> 2] = 3;
M.HEAPF64.set([0, 0, 0, 1, 0, 0, 0, 1, 0], (profile + 32) >>> 3);
const thrown = call('extrude', ctx, profile, 5.0, scratch);
console.log(`exception.degenerate_polygon\t${KINDS[thrown] ?? thrown}\t${lastError()}`);

console.error(`module instantiated in ${loadMs.toFixed(0)} ms; scenes built in ${elapsedMs.toFixed(0)} ms`);
M._w3d_occt_context_free(ctx);
