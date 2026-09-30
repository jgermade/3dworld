// The five functions `kernel-occt/src/remote.rs` asks JS for, over the
// OpenCASCADE module `make occt-wasm` builds into dist/occt/.
//
// Two wasm instances, two linear memories, and this is the whole of what joins
// them: call an entry point of `w3d_occt.h` by name, allocate and free in the
// module's heap, and copy bytes each way. It knows nothing about the header —
// every struct, pointer and ownership rule is handled on the Rust side — which
// is why it is short enough to share between the page's worker and the Node
// check (`tools/occt_bridge_check.mjs`) without either of them growing a copy.

/** The geometry format of a document only OpenCASCADE can open, as its manifest
 *  names it. `OcctKernel::GEOMETRY_FORMAT` on the Rust side. */
export const OCCT_GEOMETRY = 'occt-brep-1';

/**
 * Instantiates the module and installs the bridge on `globalThis.w3dOcct`.
 * Resolves to the milliseconds it took, fetch included — the cost a document
 * that needs OpenCASCADE pays and one that does not never does.
 *
 * `moduleUrl` is resolved against this file, so the page and a test find the
 * same build.
 */
export async function installOcct(moduleUrl = './dist/occt/w3d_occt.mjs') {
  if (globalThis.w3dOcct) return 0;
  const started = performance.now();
  let createOcct;
  try {
    ({ default: createOcct } = await import(new URL(moduleUrl, import.meta.url).href));
  } catch (e) {
    throw new Error(
      'this document was written by OpenCASCADE, and this deployment has no OpenCASCADE ' +
        `module to open it with (${moduleUrl}: ${e && e.message ? e.message : e}). ` +
        '`make occt-wasm` builds it.',
    );
  }
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
  return performance.now() - started;
}
