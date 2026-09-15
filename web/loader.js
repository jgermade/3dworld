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
    isolatedBy: isolated ? isolationSource() : null,
    // The two-variant axis, and it is an `and`. Threads in the engine without
    // the headers is the common case — a host that has not set COOP/COEP — and
    // it is exactly as unusable as no threads at all.
    threaded: threads && isolated,
  };
}

/**
 * Where the headers came from: the host, or `coi-serviceworker.js` supplying
 * what the host would not.
 *
 * It is worth showing rather than inferring. On GitHub Pages the worker is the
 * only one of the two available, and a user who cannot tell them apart cannot
 * tell a working worker from a host that quietly started sending headers — nor
 * notice when the worker stops working.
 *
 * It is a heuristic and only one case can confuse it: a page served *with* the
 * headers that is also controlled by a worker installed on an earlier visit,
 * which reads as the worker's doing. Distinguishing them would mean re-fetching
 * the document to look at its headers, and the worker rewrites those too.
 */
function isolationSource() {
  const controlled =
    typeof navigator !== 'undefined' && navigator.serviceWorker
      ? !!navigator.serviceWorker.controller
      : false;
  return controlled ? 'service worker' : 'server headers';
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
 * How many workers to ask for.
 *
 * `hardwareConcurrency` is the machine's answer and the cap is ours. The cap
 * is a **guess**, and it is written down as one: nobody has measured where
 * more workers stop paying for themselves on this workload, and starting
 * sixty-four wasm instances to mesh one solid on a machine that has sixty-four
 * cores is a cost with no evidence behind it. Replace it with a number when
 * there is a measurement — see the register's "measure something".
 */
export function poolSize(limit = 8) {
  const cores = Number(globalThis.navigator?.hardwareConcurrency);
  if (!Number.isFinite(cores) || cores < 1) return 1;
  return Math.max(1, Math.min(Math.floor(cores), limit));
}

/**
 * Chooses a variant, instantiates it, and opens a viewer on a canvas inside
 * `container`.
 *
 * Two dispatches happen here and they are independent. **Threads** picks the
 * wasm variant: `chosen` is what was picked, `wanted` is what the probe asked
 * for, and `note` says why they differ — because the threaded build is not
 * always present, and because a pool can fail to start on a page that had
 * every right to expect one. A dispatch that silently has one option is a
 * dispatch nobody notices is broken.
 *
 * **Graphics** picks WebGPU or WebGL2, and that one is not decided by a probe
 * at all — it is decided by looking at the result. See `verify` below.
 *
 * It owns the canvas rather than taking one, because a canvas keeps the first
 * context it is given: falling back from WebGPU to WebGL2 means a *new*
 * element, not a reconfigured one.
 *
 * ## The worker is the boot path
 *
 * Changed on 2026-09-15. `start` used to model the scene, tessellate every
 * body and upload the result before it returned — all of it on the thread that
 * then had to draw, which is the 56-second stall `make measure` found on a
 * real assembly. Now the mesh is made in a worker with a linear memory of its
 * own and arrives as bytes; see `web/worker.js` and `WIRE.md`.
 *
 * Three things about the order below are load-bearing:
 *
 *   - **The worker is started first**, before the module is even imported.
 *     It loads its own copy and shares nothing, so there is nothing to wait
 *     for — and every millisecond of adapter negotiation on this thread is a
 *     millisecond the worker is already modelling.
 *   - **The mesh is installed before the graphics check.** `drewSomething`
 *     asks whether WebGPU actually rasterised anything by counting colours on
 *     the canvas, and a viewer with no bodies draws a flat background. Run
 *     that check first and every browser on earth "fails" it and falls back to
 *     WebGL2. The dependency is invisible in both functions and is why it is
 *     written down here.
 *   - **The fallback re-installs, it does not re-tessellate.** The chunks are
 *     already on this thread and a `GpuMesh` is the only thing tied to the
 *     device that went away. The old code called `start` again and paid for
 *     the whole tessellation a second time, on the path taken by exactly the
 *     machines least able to afford it.
 *
 * And one thing that is not: the worker still *builds* the scene rather than
 * being sent one. That works only because the page's scene is a fixed
 * document. A modeller has to send the document across, which is a second
 * format and is not written.
 */
export async function boot(container, { chunksPerBody = 4, workerUrl } = {}) {
  const caps = probe();

  // Started before anything else on this thread, and deliberately not awaited
  // until the device is open. A rejection here is not fatal — see `meshNote`.
  //
  // `workerUrl` exists so that the failure can be *caused*. An untested
  // fallback is a fallback that does not work, and the only honest way to
  // reach this one is to give it a worker that really will not load — see
  // `web/test/browser.mjs`.
  const meshing = tessellateInWorker(chunksPerBody, undefined, workerUrl).then(
    (result) => ({ ok: true, result }),
    (error) => ({ ok: false, error }),
  );
  const wanted = caps.threaded ? 'threaded' : 'single';
  let chosen = wanted;
  let note = degradation(caps);

  if (chosen === 'threaded' && !(await exists(VARIANTS.threaded))) {
    chosen = 'single';
    note = 'The threaded variant is not built here; running single-threaded. ' +
      'This page is cross-origin isolated and the engine has threads, so the ' +
      'variant is what is missing, not the platform. `make web-threaded` ' +
      'builds it.';
  }

  let module = await import(VARIANTS[chosen]);
  await module.default();

  // The pool is started here and not inside the module: a wasm instance cannot
  // spawn its own workers, so `initThreadPool` is a promise JS has to await
  // before rayon is asked for anything. Everything downstream — the first
  // tessellation among it — runs after this line for that reason.
  //
  // It can still fail on a page that passed every probe: a Content-Security
  // -Policy without `worker-src`, a `snippets/` directory that did not get
  // deployed, an engine that runs out of memory starting instances. The
  // module is then loaded and unusable, because rayon on wasm has no
  // single-threaded fallback to panic its way into — so the single-threaded
  // variant is loaded instead, and the reason is shown rather than logged.
  if (chosen === 'threaded') {
    try {
      await module.initThreadPool(poolSize());
    } catch (e) {
      chosen = 'single';
      note =
        'The threaded variant loaded and its worker pool would not start ' +
        `(${e && e.message ? e.message : e}); running single-threaded. This ` +
        'is not a missing feature: the page is isolated and the build exists, ' +
        'so something is stopping the workers.';
      module = await import(VARIANTS.single);
      await module.default();
    }
  }

  let graphics = null;
  let { canvas, viewer } = await open(container, module, false);

  // The worker's answer, awaited here because this is the first moment the
  // result can be used: installing a mesh needs a device.
  const wire = await meshing;
  let meshNote = null;
  if (!wire.ok) {
    // A module worker can fail for reasons that are nothing to do with the
    // engine — a Content-Security-Policy without `worker-src`, a `worker.js`
    // that did not deploy, a file: URL. A blank viewport would be a worse
    // answer than a slow one, so the old path is still here and the reason is
    // shown rather than logged.
    const why = wire.error?.message ?? wire.error;
    meshNote =
      `The tessellation worker did not run (${why}), so the scene was meshed ` +
      'on the thread that draws. The page works and is slower for it; on a ' +
      'large assembly that is a visible stall.';
  }

  /** Put the mesh on whichever device is current. Called again after a
   *  graphics fallback, which is a new device and a new canvas but the same
   *  bytes — see the note about re-installing above. */
  const installMesh = (v) => {
    if (wire.ok) {
      const r = wire.result;
      return v.installWire(r.chunks, {
        tessellateMs: r.tessellateMs,
        modelled: r.modelled,
        // This thread is the only place that knows. The bytes do not say.
        fromWorker: true,
      });
    }
    return v.tessellateHere();
  };
  installMesh(viewer);

  // The check that `navigator.gpu` cannot answer. A browser can report WebGPU,
  // hand back an adapter with generous limits, accept every command — and
  // rasterise nothing, which reaches a user as a black canvas and no error.
  // Headless Chromium without a working GPU does exactly this. So the first
  // frame is looked at, and WebGL2 is a fallback from *evidence* rather than
  // from a feature flag.
  //
  // It runs *after* the mesh is installed because an empty viewport is a flat
  // canvas, which is precisely what this function reads as "drew nothing".
  if (!drewSomething(canvas, viewer)) {
    graphics =
      'WebGPU reported an adapter and then drew nothing; fell back to WebGL2. ' +
      'This is a browser or driver fault, not a missing feature.';
    container.removeChild(canvas);
    ({ canvas, viewer } = await open(container, module, true));
    installMesh(viewer);
  }

  return {
    // `module` goes out so that a caller can tessellate on *this* thread and
    // compare. That is the check the worker boundary used to get for free,
    // when the main thread meshed at startup and the worker's result replaced
    // it; now that the worker went first, the reference has to be made on
    // purpose. See `web/test/browser.mjs`.
    viewer, canvas, module, caps, wanted, chosen, note, graphics, meshNote,
    wire: wire.ok ? wire.result : null,
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

/**
 * Runs a tessellation in a worker and brings the result back as bytes.
 *
 * The one place a message described by `WIRE.md` actually crosses a heap. The
 * worker is a *second instantiation* of the same wasm module with a linear
 * memory of its own — see `web/worker.js` — so what comes back cannot be an
 * object, a pointer or a `Vec`. It is `ArrayBuffer`s, and they arrive
 * **transferred**: the worker no longer has them.
 *
 * That last part is checked rather than assumed. A `postMessage` whose second
 * argument is wrong still delivers, by *copying*, and the difference is
 * invisible from here — so the worker sends the byte count it intended to give
 * up, and `transferred` says whether the buffers really moved. A boundary that
 * silently copies 84 MiB is the failure this whole design exists to avoid, and
 * nothing else in the stack would report it.
 *
 * Resolves with `{ chunks, bytes, tessellateMs, initMs, transferred, modelled }`.
 * `modelled` is the worker's answer to whether the scene's boolean actually
 * cut anything — a fact about *its* document, which since this became the boot
 * path is the only one there is.
 */
export function tessellateInWorker(chunksPerBody = 4, timeoutMs = 60000, workerUrl = './worker.js') {
  return new Promise((resolve, reject) => {
    let worker;
    try {
      worker = new Worker(new URL(workerUrl, import.meta.url), { type: 'module' });
    } catch (e) {
      reject(new Error(`could not start the tessellation worker: ${e.message ?? e}`));
      return;
    }

    const timer = setTimeout(() => {
      worker.terminate();
      reject(new Error(`the tessellation worker did not answer in ${timeoutMs} ms`));
    }, timeoutMs);

    // Two messages arrive: the payload, then the worker's verdict on whether
    // its own buffers were detached by the post. The second is the only
    // evidence that the transfer was a move — see `web/worker.js`.
    let payload = null;
    worker.onmessage = (event) => {
      const msg = event.data;
      if (!msg || !msg.ok) {
        clearTimeout(timer);
        worker.terminate();
        reject(new Error(msg?.error ?? 'the tessellation worker failed and said nothing'));
        return;
      }
      if (!msg.verdict) {
        payload = msg;
        return;
      }

      clearTimeout(timer);
      worker.terminate();
      if (!payload) {
        reject(new Error('the worker reported on a payload it never sent'));
        return;
      }
      const chunks = payload.buffers.map((b) => new Uint8Array(b));
      const arrived = chunks.reduce((n, c) => n + c.length, 0);
      resolve({
        chunks,
        bytes: arrived,
        claimed: payload.bytes,
        initMs: payload.initMs,
        totalMs: payload.totalMs,
        modelMs: payload.modelMs,
        tessellateMs: payload.tessellateMs,
        encodeMs: payload.encodeMs,
        modelled: payload.modelled === true,
        transferred: msg.transferredOk === true,
      });
    };

    worker.onerror = (e) => {
      clearTimeout(timer);
      worker.terminate();
      // A module worker that 404s fires an `ErrorEvent` carrying nothing: no
      // message, no filename, no line. `String(e)` on it reads `[object
      // Event]`, which told the first version of this exactly nothing — so the
      // URL that was asked for is the fact worth reporting, because a worker
      // that did not deploy is the likeliest way to arrive here.
      const detail = e?.message || `could not load ${workerUrl}`;
      reject(new Error(`the tessellation worker failed to load: ${detail}`));
    };

    worker.postMessage({ chunksPerBody });
  });
}
