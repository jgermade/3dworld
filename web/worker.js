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
    const { chunks, modelMs, tessellateMs, encodeMs } = result;

    // `tessellateScene` returns `Uint8Array`s that are *views into this
    // module's linear memory*. Transferring the memory itself is neither
    // possible nor wanted, so each one is copied into a buffer of its own
    // first — and that copy is what then moves for free.
    //
    // Doing it the other way round is the bug this comment exists to prevent:
    // posting the views' `.buffer` would hand over the whole wasm memory, which
    // the engine refuses, and on an engine that did not, would detach the heap
    // this worker is still running in.
    const buffers = chunks.map((chunk) => {
      const copy = new Uint8Array(chunk.length);
      copy.set(chunk);
      return copy.buffer;
    });

    const bytes = buffers.reduce((n, b) => n + b.byteLength, 0);
    self.postMessage(
      { ok: true, buffers, bytes, initMs, totalMs, modelMs, tessellateMs, encodeMs },
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
