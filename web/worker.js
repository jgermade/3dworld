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
 * ## It is sent a document, or it builds one
 *
 * Both, and which one matters. Until 2026-09-15 this worker could only *build*
 * the page's scene, which worked because that scene is fixed and both sides
 * can construct it — and which is exactly what kept it a demonstration rather
 * than a modeller's boot path. The moment a user opens a file, the thread that
 * draws is holding bytes and the worker has to be handed them.
 *
 * So it now takes a document: a `.w3d`, posted as an `ArrayBuffer` and
 * transferred like everything else here. **It is not a second format.** `.w3d`
 * is what `FORMAT.md` already specifies, `w3d-format` already implements, and
 * — usefully — depends on nothing that fails to build for wasm32, its zip
 * being its own. A document crossing a worker boundary and a document crossing
 * a disk turn out to be the same problem.
 *
 * The asymmetry is deliberate and worth naming: **bytes in are a model, bytes
 * out are triangles.** `WIRE.md` describes a mesh and cannot describe a solid;
 * `FORMAT.md` describes a solid and says nothing about how to draw it. Sending
 * a `.w3d` back would mean the worker had done no work.
 *
 * With no document it falls back to the built-in scene, which is what happens
 * when `make web-scene` has not run. `report().source` says which, and the
 * browser test asserts both branches — a boot path that quietly stopped using
 * the document would otherwise draw exactly the same picture.
 */

import init, { tessellateScene, tessellateDocument } from './dist/w3d_web.js';

self.onmessage = async (event) => {
  const { chunksPerBody = 4, document = null } = event.data ?? {};
  try {
    const startedInit = performance.now();
    await init();
    const initMs = performance.now() - startedInit;

    const started = performance.now();
    // A document if the page had one to send, the compiled-in scene if not.
    // The second is the older path and is still the honest answer when
    // `make web-scene` has not run — not a silent one: `source` travels back.
    const source = document ? 'document' : 'built-in scene';
    const result = document
      ? tessellateDocument(new Uint8Array(document), chunksPerBody)
      : tessellateScene(chunksPerBody);
    const totalMs = performance.now() - started;
    const { chunks, bodies, modelMs, tessellateMs, encodeMs, modelled } = result;

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
      {
        ok: true, buffers, bytes, bodies, source,
        initMs, totalMs, modelMs, tessellateMs, encodeMs,
        // `null` from the document path, and that is the answer: a solid does
        // not record whether a boolean made it. See `tessellate_document`.
        modelled,
      },
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
