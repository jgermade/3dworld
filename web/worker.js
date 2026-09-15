/**
 * A worker that tessellates, and the only place in this repository where a
 * tessellation actually crosses a heap.
 *
 * ## Why this file is a worker and not a function
 *
 * `STACK.md` decides that bulk work runs in a worker with **its own linear
 * memory**, so that the total addressable memory exceeds wasm32's 4 GB ceiling
 * without paying for 64-bit pointers. That is what this is: a *second
 * instantiation* of the same module, with a memory of its own, which is why it
 * loads `./dist/w3d_web.js` — the single-threaded variant — and not the
 * threaded one. The threaded build imports a shared memory; sharing one is the
 * opposite of what this worker is for.
 *
 * It follows that nothing here can hand back a pointer, a `Vec`, or an object
 * from that heap. What it hands back is bytes, laid out as `WIRE.md` specifies,
 * and posted with the buffers in the transfer list so the move is a move.
 *
 * ## What it does not do
 *
 * It builds the scene itself rather than being sent one. The page's scene is a
 * fixed document — see `scene()` in `web/src/lib.rs` — so both sides can
 * construct it and the boundary is still exercised honestly. A worker that is
 * sent a *document* needs the document to be serialisable across the same
 * boundary, which is a second format and a later problem.
 *
 * That limit is the whole of what keeps this a *demonstration* boot path
 * rather than a modeller's. Since 2026-09-15 this worker is what the page
 * boots on — nothing is tessellated on the thread that draws any more — but it
 * can only be, because the one scene there is happens to be constructible from
 * nothing. The moment a user opens a file, the document has to cross.
 */

import init, { tessellateScene } from './dist/w3d_web.js';

self.onmessage = async (event) => {
  const { chunksPerBody = 4 } = event.data ?? {};
  try {
    const startedInit = performance.now();
    await init();
    const initMs = performance.now() - startedInit;

    const started = performance.now();
    const result = tessellateScene(chunksPerBody);
    const totalMs = performance.now() - started;
    const { chunks, modelMs, tessellateMs, encodeMs, modelled } = result;

    // Each chunk is copied into a buffer of its own before being posted, and
    // that copy is what then moves for free.
    //
    // **The reason is not the one this comment used to give.** It said these
    // were views into the module's linear memory, so that posting `.buffer`
    // would offer the engine the whole wasm heap. Measured on 2026-09-15, in
    // the run `make web-test` now makes, that is not what js-sys hands back:
    // a 75 964-byte chunk arrived in a 75 964-byte `ArrayBuffer`, so
    // `Uint8Array::from` is copying onto the JS heap and each chunk already
    // owns its buffer. `web/test/browser.mjs` asserts that ratio, so a change
    // to it is a failing check rather than a silent hazard.
    //
    // The copy stays, as insurance rather than necessity, because what it
    // guards against is real and one line away: the day `tessellateScene`
    // hands back a `Uint8Array::view` — which is the obvious thing to reach
    // for to avoid a copy — posting `.buffer` would offer the entire linear
    // memory, and on an engine that allowed it, detach the heap this worker is
    // still running in. It costs one memcpy of 178 KiB against half a second
    // of meshing.
    //
    // What is still not explained is why the *first* version of this file
    // failed in Chromium, which is what the original comment was written from.
    // The measurement says what the buffers are; it does not say what went
    // wrong that day, and nobody should assume this paragraph settles it.
    const buffers = chunks.map((chunk) => {
      const copy = new Uint8Array(chunk.length);
      copy.set(chunk);
      return copy.buffer;
    });

    const bytes = buffers.reduce((n, b) => n + b.byteLength, 0);
    self.postMessage(
      // `modelled` travels beside the bytes rather than in them: `WIRE.md`
      // describes a mesh and has no field for whether a boolean succeeded, and
      // the page has no document of its own left to ask.
      { ok: true, buffers, bytes, initMs, totalMs, modelMs, tessellateMs, encodeMs, modelled },
      buffers,
    );

    // The proof that the post was a *move* and not a copy, and it can only be
    // made here: a transferred `ArrayBuffer` is detached on the sending side,
    // and a detached buffer's `byteLength` is 0. A `postMessage` with a wrong
    // transfer list still delivers — by cloning — and from the receiving end
    // the two are indistinguishable. So the sender looks at what it has left.
    //
    // It goes in a second message because the first one is already gone.
    const transferredOk = buffers.every((b) => b.byteLength === 0);
    self.postMessage({ ok: true, verdict: true, transferredOk, bytes });
  } catch (e) {
    // A worker that throws is a page that waits forever. The failure has to
    // come back as a message like any other answer.
    self.postMessage({ ok: false, error: String((e && e.message) || e) });
  }
};
