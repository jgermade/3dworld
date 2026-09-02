// Cross-origin isolation on a host that will not send the headers.
//
// GitHub Pages serves static files and offers no way to add a response header.
// `Cross-Origin-Opener-Policy` and `Cross-Origin-Embedder-Policy` therefore
// never arrive, `SharedArrayBuffer` is unavailable, and the page lands on the
// exact branch `loader.js` probes for: threads in the engine, no isolation on
// the page, single-threaded modeller and a line saying why.
//
// A service worker can supply what the host will not. Once one controls the
// page, every response is re-issued through its `fetch` handler, and a
// re-issued response can carry headers the server never sent. The browser then
// treats the document as cross-origin isolated, because from where it stands
// the headers were there.
//
// The technique is gzuidhof/coi-serviceworker's, and this file is not a copy
// of it: it is the same idea written small enough to read, with the parts that
// matter here argued rather than assumed. What it costs is worth stating in
// full, because none of it is hypothetical:
//
//   - **It cannot work on the first load.** A worker does not control the
//     navigation that registered it. The first visit is served without the
//     headers and reloads once, under the worker, to get them. That reload is
//     the price, and this file spends it *before* the wasm is fetched — see
//     `ready` below — rather than instantiating a modeller it is about to
//     throw away.
//   - **`require-corp` is a constraint on the whole page from now on.** Every
//     same-origin subresource is fine. A cross-origin one needs CORP or CORS
//     from its own host, or it is blocked — and blocked by a policy this file
//     introduced, which is a debugging session nobody enjoys. This page loads
//     nothing cross-origin today. Keep it that way, or set the crossorigin
//     attribute knowingly.
//   - **It can be off.** No service workers in a private window in some
//     browsers, none on an insecure origin, none if the user turned them off.
//     Every one of those ends with the page running single-threaded and saying
//     so, which is the same outcome as a host that forgot the headers.
//
// The alternative, for the record, is a host that can send headers — Cloudflare
// Pages and Netlify both take a `_headers` file — and it is strictly better
// than this when it is available. On GitHub Pages it is not available at all.

const RELOAD_KEY = 'w3d-coi-reloads';

// One reload gets the worker in control. A second covers the case where the
// first raced an install. A third would mean the headers are not taking effect
// and reloading is not going to change that, so the page runs degraded and
// says why — a reload loop is worse than a slow modeller, and much harder to
// diagnose from the outside.
const RELOAD_LIMIT = 2;

// How long to wait for the worker before booting anyway. A registration that
// hangs must not leave a blank page; STACK.md's rule is that the failure is
// visible, and a spinner is not visible.
const SETTLE_MS = 4000;

if (typeof window === 'undefined') {
  installServiceWorker();
} else {
  window.__w3dCOI = registerServiceWorker();
}

// ---- The page's half: register, and decide whether to reload ---------------

function registerServiceWorker() {
  const state = {
    // Whether the page is isolated *now*, before anything is registered.
    isolated: !!window.crossOriginIsolated,
    // Why it is not, or how it came to be. Shown on the page.
    note: null,
    // Resolves when the caller should get on with booting. On the path that
    // reloads it deliberately never resolves: the page is going away, and
    // starting a wasm instantiation it cannot finish wastes a download.
    ready: null,
  };

  const settled = (note) => {
    state.note = note;
    return Promise.resolve();
  };

  if (state.isolated) {
    // Already isolated: either the host sent the headers, or this is the
    // reload and the worker is doing it. Clear the counter so the next visit
    // starts with its full budget.
    try {
      sessionStorage.removeItem(RELOAD_KEY);
    } catch {
      /* storage can be denied; the counter is an optimisation, not a rule */
    }
    state.ready = settled(null);
    return state;
  }

  // A testing hook, and a debugging one: `?coi=off` skips registration, which
  // is how `web/test/browser.mjs` still gets to assert that a page with no
  // headers and no worker degrades visibly. It does not *remove* a worker that
  // is already installed — that would need `unregister()`, and a flag that
  // silently uninstalls things is a worse hook than one that does not.
  if (new URLSearchParams(location.search).get('coi') === 'off') {
    state.ready = settled('Cross-origin isolation is disabled by ?coi=off.');
    return state;
  }

  if (!window.isSecureContext || !('serviceWorker' in navigator)) {
    state.ready = settled(
      'This browser will not run a service worker here, so the COOP/COEP ' +
        'headers this host does not send cannot be supplied either.',
    );
    return state;
  }

  const reloads = reloadCount();
  if (reloads >= RELOAD_LIMIT) {
    state.ready = settled(
      'A service worker is installed and the page is still not cross-origin ' +
        'isolated after reloading. Not reloading again.',
    );
    return state;
  }

  // `document.currentScript` is the script element *while it runs*, which is
  // why this file has to be a classic script and not a module: in a module it
  // is null, and registering the wrong URL registers a worker with the wrong
  // scope. Reading it here, synchronously, is the only place it is valid.
  const src = document.currentScript && document.currentScript.src;
  if (!src) {
    state.ready = settled(
      'coi-serviceworker.js must be loaded as a classic <script src=…>; it ' +
        'cannot find its own URL otherwise.',
    );
    return state;
  }

  state.ready = new Promise((resolve) => {
    // The registration is not allowed to hold the page hostage.
    const giveUp = setTimeout(() => {
      state.note =
        'The service worker did not install in time; continuing without ' +
        'cross-origin isolation.';
      resolve();
    }, SETTLE_MS);

    navigator.serviceWorker
      .register(src)
      .then(() => {
        if (navigator.serviceWorker.controller) {
          // A worker is already in charge and the page is still not isolated,
          // on a load that did not reload to get here. Reloading would change
          // nothing.
          clearTimeout(giveUp);
          state.note =
            'A service worker controls this page and it is still not ' +
            'cross-origin isolated. The headers are being stripped or ' +
            'ignored.';
          resolve();
          return;
        }
        // `ready` resolves once the registration has an active worker. A
        // navigation from that point on is controlled, which is the whole
        // trick: the reload is what gets the headers, not the registration.
        return navigator.serviceWorker.ready.then(() => {
          bumpReloadCount(reloads);
          location.reload();
          // Deliberately no resolve(): the page is leaving.
        });
      })
      .catch((err) => {
        clearTimeout(giveUp);
        state.note = `The service worker could not be registered: ${err}`;
        resolve();
      });
  });

  return state;
}

function reloadCount() {
  try {
    return Number(sessionStorage.getItem(RELOAD_KEY)) || 0;
  } catch {
    // Storage denied. Treat it as the first reload every time rather than as
    // none: the budget exists to stop a loop, and a browser that cannot count
    // gets the smaller budget, not the unbounded one.
    return RELOAD_LIMIT - 1;
  }
}

function bumpReloadCount(reloads) {
  try {
    sessionStorage.setItem(RELOAD_KEY, String(reloads + 1));
  } catch {
    /* see above */
  }
}

// ---- The worker's half: re-issue every response with the headers ----------

function installServiceWorker() {
  // Take over immediately rather than waiting for every other tab to close.
  // The page that registered this worker is reloading to be controlled by it;
  // a worker that waits makes that reload pointless.
  self.addEventListener('install', () => self.skipWaiting());
  self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));

  self.addEventListener('fetch', (event) => {
    const request = event.request;

    // A cache-only request that is not same-origin cannot be served by
    // `fetch()` and throws if we try. Let the browser deal with it.
    if (request.cache === 'only-if-cached' && request.mode !== 'same-origin') return;

    event.respondWith(
      fetch(request)
        .then((response) => {
          // An opaque response has no readable body and no headers to copy;
          // `new Response` on one throws. Pass it through untouched — under
          // `require-corp` the browser blocks it unless its own host sent
          // CORP, which is exactly the policy we just asked for.
          if (response.status === 0) return response;

          const headers = new Headers(response.headers);
          headers.set('Cross-Origin-Embedder-Policy', 'require-corp');
          headers.set('Cross-Origin-Opener-Policy', 'same-origin');
          return new Response(response.body, {
            status: response.status,
            statusText: response.statusText,
            headers,
          });
        })
        .catch((err) => {
          // `respondWith` given a promise that resolves to `undefined` fails
          // with a TypeError about the argument, which reaches the page as a
          // network error with a misleading message. `Response.error()` is
          // the network error the request was going to be anyway.
          console.error('coi-serviceworker: fetch failed', request.url, err);
          return Response.error();
        }),
    );
  });
}
