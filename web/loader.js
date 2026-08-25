// The loader: probe first, then choose, and say which was chosen.
//
// There is no CPUID inside wasm — a module cannot ask what the machine can do —
// so detection happens out here, before instantiation, by validating probe
// modules. That is why this file is JavaScript and not Rust: by the time Rust
// is running, the choice has already been made.
//
// Two things are probed and they are not the same question:
//
//   - **Threads** are a property of the *engine*: does it accept a module with
//     a shared memory and atomic instructions.
//   - **Cross-origin isolation** is a property of the *page*: did the server
//     send COOP and COEP, without which `SharedArrayBuffer` is unavailable
//     however capable the engine is.
//
// A host that forgets the headers gets a working single-threaded modeller and
// a visible line saying why it is slower. It does not get a blank page, and it
// does not get silence.

/** A module with a shared memory and an `i32.atomic.load`. Validates only
 *  where the threads proposal is implemented. Same bytes wasm-feature-detect
 *  uses; kept inline so the loader has no dependency to fetch before it can
 *  decide anything. */
const THREADS_PROBE = new Uint8Array([
  0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic, version
  0x01, 0x04, 0x01, 0x60, 0x00, 0x00,             // type:   () -> ()
  0x03, 0x02, 0x01, 0x00,                         // func:   one, of that type
  0x05, 0x04, 0x01, 0x03, 0x01, 0x01,             // memory: shared, min 1 max 1
  0x0a, 0x0b, 0x01, 0x09, 0x00,                   // code
  0x41, 0x00,                                     //   i32.const 0
  0xfe, 0x10, 0x02, 0x00,                         //   i32.atomic.load
  0x1a, 0x0b,                                     //   drop, end
]);

/** `v128` — the SIMD128 baseline. Not a branch in the build matrix; STACK.md
 *  takes it as universal since Safari 16.4. Probed anyway, because "universal"
 *  is a claim and a user on something older deserves a message rather than a
 *  `LinkError`. */
const SIMD_PROBE = new Uint8Array([
  0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
  0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7b,       // type: () -> v128
  0x03, 0x02, 0x01, 0x00,                         // func: one, of that type
  0x0a, 0x16, 0x01, 0x14, 0x00,                   // code
  0xfd, 0x0c,                                     //   v128.const, and its
  0, 0, 0, 0, 0, 0, 0, 0,                         //   sixteen bytes of
  0, 0, 0, 0, 0, 0, 0, 0,                         //   immediate
  0x0b,                                           //   end
]);

/** What this engine and this page can do, before anything is instantiated. */
export function probe() {
  const threads = WebAssembly.validate(THREADS_PROBE);
  const simd = WebAssembly.validate(SIMD_PROBE);
  // `crossOriginIsolated` is the honest question. `typeof SharedArrayBuffer`
  // is not: some engines define the constructor and refuse to let a memory be
  // shared, which fails at instantiation instead of here.
  const isolated = typeof crossOriginIsolated === 'boolean'
    ? crossOriginIsolated
    : typeof SharedArrayBuffer === 'function';

  return {
    threads,
    simd,
    isolated,
    // The two-variant axis, and it is an `and`. Threads in the engine without
    // the headers is the common case — a host that has not set COOP/COEP — and
    // it is exactly as unusable as no threads at all.
    threaded: threads && isolated,
  };
}

/** Why the fast variant was not used, in one sentence, or null. */
export function degradation(caps) {
  if (caps.threaded) return null;
  if (!caps.threads) {
    return 'This browser has no WebAssembly threads. Large booleans and ' +
      'imports run on one core.';
  }
  return 'This page is not cross-origin isolated: the server did not send ' +
    'Cross-Origin-Opener-Policy: same-origin and ' +
    'Cross-Origin-Embedder-Policy: require-corp, so SharedArrayBuffer is ' +
    'unavailable and the modeller runs on one core.';
}

/** The two entries of the build matrix, kept to two on purpose — every entry
 *  is payload every user might download. */
const VARIANTS = {
  threaded: './dist/threaded/w3d_web.js',
  single: './dist/w3d_web.js',
};

/**
 * Chooses a variant, instantiates it, and opens a viewer on a canvas inside
 * `container`.
 *
 * Two dispatches happen here and they are independent. **Threads** picks the
 * wasm variant, and the threaded one does not exist yet — which is stated
 * rather than hidden: `chosen` is what was picked, `wanted` is what the probe
 * asked for, and `note` says why they differ. A dispatch that silently has one
 * option is a dispatch nobody notices is broken.
 *
 * **Graphics** picks WebGPU or WebGL2, and that one is not decided by a probe
 * at all — it is decided by looking at the result. See `verify` below.
 *
 * It owns the canvas rather than taking one, because a canvas keeps the first
 * context it is given: falling back from WebGPU to WebGL2 means a *new*
 * element, not a reconfigured one.
 */
export async function boot(container) {
  const caps = probe();
  const wanted = caps.threaded ? 'threaded' : 'single';
  let chosen = wanted;
  let note = degradation(caps);

  if (chosen === 'threaded' && !(await exists(VARIANTS.threaded))) {
    chosen = 'single';
    note = 'The threaded variant is not built yet; running single-threaded. ' +
      'This page is cross-origin isolated and the engine has threads, so the ' +
      'variant is what is missing, not the platform.';
  }

  const module = await import(VARIANTS[chosen]);
  await module.default();

  let graphics = null;
  let { canvas, viewer } = await open(container, module, false);

  // The check that `navigator.gpu` cannot answer. A browser can report WebGPU,
  // hand back an adapter with generous limits, accept every command — and
  // rasterise nothing, which reaches a user as a black canvas and no error.
  // Headless Chromium without a working GPU does exactly this. So the first
  // frame is looked at, and WebGL2 is a fallback from *evidence* rather than
  // from a feature flag.
  if (!drewSomething(canvas, viewer)) {
    graphics =
      'WebGPU reported an adapter and then drew nothing; fell back to WebGL2. ' +
      'This is a browser or driver fault, not a missing feature.';
    container.removeChild(canvas);
    ({ canvas, viewer } = await open(container, module, true));
  }

  return {
    viewer, canvas, caps, wanted, chosen, note, graphics,
    report: viewer.report(),
  };
}

async function open(container, module, forceWebgl) {
  const canvas = document.createElement('canvas');
  canvas.width = container.clientWidth || 960;
  canvas.height = container.clientHeight || 600;
  container.appendChild(canvas);
  const viewer = await module.start(canvas, forceWebgl);
  return { canvas, viewer };
}

/**
 * How many distinct colours a canvas is showing, sampled small.
 *
 * **Must be called in the same task as the render that produced the frame.**
 * A WebGL2 drawing buffer is not preserved: once the frame has been
 * composited, `drawImage` reads back a cleared buffer and every canvas looks
 * blank. That is a property of the platform, not a bug here, and it is the
 * reason this is exported — a caller that wants to know must render and
 * sample together.
 *
 * Compositing through a 2d context is the only way to read a canvas whose
 * context belongs to wgpu.
 */
export function distinctColours(canvas, w = 48, h = 32) {
  const probeCanvas = document.createElement('canvas');
  probeCanvas.width = w;
  probeCanvas.height = h;
  const g = probeCanvas.getContext('2d', { willReadFrequently: true });
  if (!g) return Infinity; // Cannot tell; never conclude "blank" from ignorance.
  g.drawImage(canvas, 0, 0, w, h);

  const pixels = g.getImageData(0, 0, w, h).data;
  const seen = new Set();
  for (let i = 0; i < pixels.length; i += 4) {
    seen.add(`${pixels[i]},${pixels[i + 1]},${pixels[i + 2]}`);
  }
  return seen.size;
}

/** The scene at startup is a lit solid on a dark background, so one flat
 *  colour means nothing was drawn. An empty document would be flat too — which
 *  is why this runs against the fixed startup scene and not against whatever
 *  the user has open. */
function drewSomething(canvas, viewer) {
  for (let i = 0; i < 3; i += 1) viewer.render();
  return distinctColours(canvas) > 1;
}

async function exists(url) {
  try {
    const res = await fetch(url, { method: 'HEAD' });
    return res.ok;
  } catch {
    return false;
  }
}
