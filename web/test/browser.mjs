// The only check in this repository that runs the viewport in a browser.
//
// Everything else is native, and native proves the pipeline but not the
// platform: WebGL2 in particular has no compute shaders, a different set of
// renderable formats and its own idea of what a downlevel limit is. Until this
// file existed, every claim in STACK.md about the fallback was an argument.
//
// It runs the page five times, and the five are different questions:
//
//   1. WebGPU offered — whatever the page ends up on, it must draw and pick.
//      Headless Chromium here reports WebGPU and then rasterises nothing, so
//      this is also the test of the loader's fall back *from evidence*.
//   2. WebGL2, by launching a browser with no `navigator.gpu` at all, so the
//      fallback is exercised rather than described.
//   3. No COOP/COEP and no service worker — the loader must degrade *visibly*,
//      which is a rule in STACK.md and otherwise nothing checks it.
//   4. The worker boundary, which since 2026-09-15 is the **boot path**: the
//      page tessellates nothing on the thread that draws. A second wasm
//      instantiation, with a linear memory of its own, models and meshes and
//      posts the result as transferable buffers, and the viewer is built from
//      those bytes and nothing else. See WIRE.md.
//
//      This run also builds the reference the old arrangement got for free.
//      While the main thread meshed at startup and the worker's result
//      replaced it, "the same triangles by a different route" was a
//      before-and-after. Now the worker goes first, so the run tessellates the
//      same scene on the main thread afterwards and requires the picture not
//      to change — the same claim, made in the opposite direction.
//   5. The worker failing. The boot path's fallback — meshing on the thread
//      that draws — is reached by pointing the loader at a worker that does
//      not exist, so the failure is real rather than a flag that skips the
//      attempt. An untested fallback is a fallback that does not work, and
//      this one exists precisely for the machines nobody here is testing on.
//   6. No COOP/COEP, service worker allowed — the case every GitHub Pages
//      visitor is in. `coi-serviceworker.js` must supply the headers the host
//      never sends, and the page must end up isolated and say where the
//      isolation came from. Without this run the service worker is a claim.
//
// Runs 1 and 6 also decide the variant, and what they assert depends on
// whether `make web-threaded` has run — see `THREADED_BUILT`. Both branches
// assert something, because the interesting failure is a page that boots,
// prints "threaded" and runs on one core.
//
// Needs `npm install` in this directory, and `make web` to have run.
// `make web-threaded` as well, to exercise the threaded half.

import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

import fs from 'node:fs';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..', '..');
const require = createRequire(import.meta.url);

let chromium;
try {
  ({ chromium } = require('playwright'));
} catch {
  console.error(
    'playwright is not installed. `cd web/test && npm install`, or set ' +
      'NODE_PATH to a directory that has it.',
  );
  process.exit(2);
}

/** The pre-installed browser, if there is one. Playwright otherwise looks for
 *  a build matching its own version, which is not necessarily what is here. */
const OPT_CHROME = '/opt/pw-browsers/chromium-1194/chrome-linux/chrome';
const EXECUTABLE = process.env.W3D_CHROME ?? (fs.existsSync(OPT_CHROME) ? OPT_CHROME : undefined);

/**
 * Whether `make web-threaded` has run. The threaded build needs a nightly
 * toolchain and a rebuilt `std`, so it is not a precondition of this file —
 * but its absence must not read as a pass. Every assertion below that depends
 * on it has *two* branches, and both of them assert: with the build, the page
 * must end up threaded with a pool bigger than one; without it, the page must
 * say so in the note the loader writes. A check that quietly evaporates when
 * an artifact is missing is the failure mode this whole directory exists to
 * avoid.
 */
const THREADED_BUILT = fs.existsSync(
  path.join(root, 'web', 'dist', 'threaded', 'w3d_web.js'),
);

const failures = [];

function check(name, ok, detail = '') {
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${name}${detail ? ` — ${detail}` : ''}`);
  if (!ok) failures.push(name);
}

/** What must be true of the variant on a page that is cross-origin isolated
 *  and has threads in the engine — under either build. */
function checkVariant(r) {
  if (THREADED_BUILT) {
    check('the threaded variant was chosen', r.chosen === 'threaded', r.chosen);
    // The number rayon reports, not the directory the module came from. A pool
    // that failed to start still loads a module called `threaded`.
    check(
      'and its worker pool really started',
      r.report.threads > 1,
      `pool of ${r.report.threads}`,
    );
    check(
      'so nothing needed explaining',
      r.note === null,
      r.note ?? '(no note)',
    );
  } else {
    check(
      'the variant is single, and the reason is the missing build',
      r.chosen === 'single' && (r.note ?? '').includes('not built here'),
      r.note ?? '(no note)',
    );
    check(
      'and the single-threaded build reports a pool of one',
      r.report.threads === 1,
      `pool of ${r.report.threads}`,
    );
  }
}

function serve({ isolated }) {
  const args = [path.join(root, 'web', 'serve.py'), '--port', '0'];
  if (!isolated) args.push('--no-isolation');
  const proc = spawn('python3', args, { stdio: ['ignore', 'pipe', 'inherit'] });
  return new Promise((resolve, reject) => {
    let out = '';
    proc.stdout.on('data', (chunk) => {
      out += chunk;
      const url = out.match(/http:\/\/[\d.]+:(\d+)\//);
      if (url) resolve({ proc, url: url[0] });
    });
    proc.on('exit', (code) => reject(new Error(`serve.py exited ${code}`)));
    setTimeout(() => reject(new Error('serve.py did not report a port')), 10000);
  });
}

/** Loads the page and returns everything it learned, plus a screenshot of the
 *  canvas. Nothing here inspects internals the page does not itself display. */
async function run({
  isolated = true,
  webgpu = true,
  coi = true,
  compare = false,
  breakWorker = false,
} = {}) {
  const { proc, url } = await serve({ isolated });
  // `?coi=off` is the page's own hook for skipping service-worker registration.
  // Run 3 needs it: with the worker in play a host that sends no headers is no
  // longer a page that cannot be isolated, and the degradation it is there to
  // check never happens.
  const params = [];
  if (!coi) params.push('coi=off');
  // The page's own hook: it boots against a worker URL that 404s, so the
  // rejection `boot` handles is the one a broken deployment would produce.
  if (breakWorker) params.push('worker=fail');
  const target = params.length ? `${url}?${params.join('&')}` : url;
  const args = ['--no-sandbox', '--enable-unsafe-swiftshader'];
  if (webgpu) {
    args.push('--enable-unsafe-webgpu', '--enable-features=Vulkan');
  } else {
    // No `navigator.gpu` at all. wgpu's own WebGPU detection then drops to
    // WebGL2, which is the code path a user on Firefox ESR or Safari 17 takes.
    args.push('--disable-features=WebGPU,WebGPUExperimentalFeatures');
  }

  const launchOptions = { args };
  if (EXECUTABLE) {
    launchOptions.executablePath = EXECUTABLE;
  }
  const browser = await chromium.launch(launchOptions);
  try {
    const page = await browser.newPage({ viewport: { width: 960, height: 660 } });
    const consoleErrors = [];
    page.on('pageerror', (e) => consoleErrors.push(String(e)));
    await page.goto(target, { waitUntil: 'load' });

    await page
      .waitForFunction(() => globalThis.__w3d?.ready || globalThis.__w3d?.error, null, {
        timeout: 30000,
      })
      .catch(() => {});

    const state = await page.evaluate(() => {
      const s = globalThis.__w3d ?? {};
      return {
        ready: !!s.ready,
        error: s.error ?? null,
        caps: s.caps ?? null,
        report: s.report ?? null,
        graphics: s.graphics ?? null,
        chosen: s.chosen ?? null,
        wanted: s.wanted ?? null,
        note: s.note ?? null,
        // What the boot path's worker reported, or null if it never ran.
        wire: s.wire ?? null,
        meshNote: s.meshNote ?? null,
        status: document.getElementById('status')?.textContent ?? '',
      };
    });

    let pick = null;
    let colours = 0;
    let canvasFit = null;
    if (state.ready) {
      // Let the loop run so `frames` is a rendered frame count, not zero.
      await page.waitForFunction(() => globalThis.__w3d.frames > 2, null, { timeout: 10000 });
      // Asked of the page, not taken from a screenshot. Two reasons: a PNG's
      // byte count is a poor proxy — a blank 960x660 frame still encodes to a
      // few kilobytes, which is how the first version of this file reported a
      // black canvas as drawn — and a WebGL2 drawing buffer is empty by the
      // time anything outside the rendering task looks at it.
      colours = await page.evaluate(() => globalThis.__w3d.sample());

      // Captured before the click, because it is what decides whether the
      // click means anything. See `checkCanvasFit`.
      canvasFit = await page.evaluate(() => {
        const c = document.querySelector('#viewport canvas');
        return c && { backing: [c.width, c.height], css: [c.clientWidth, c.clientHeight] };
      });

      const box = await page.locator('#viewport').boundingBox();
      await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
      pick = await page
        .waitForFunction(() => globalThis.__w3d.lastPick ?? null, null, { timeout: 10000 })
        .then((h) => h.jsonValue())
        .catch(() => null);
      state.frames = await page.evaluate(() => globalThis.__w3d.frames);
    }

    // Run last, because it replaces the bodies on the viewer: everything
    // above is measured against what the *worker* produced at boot, which is
    // the thing under test.
    let comparison = null;
    if (compare && state.ready) {
      comparison = await page
        .evaluate(() => globalThis.__w3d.compareWithMainThread(4))
        .catch((e) => ({ failed: String(e && e.message ? e.message : e) }));
    }

    return { ...state, pick, colours, canvasFit, comparison, consoleErrors };
  } finally {
    await browser.close();
    proc.kill();
  }
}

/** A lit solid on a dark background is many colours. One is a blank canvas. */
const DRAWN = 8;

/**
 * The canvas's backing store must be the size the canvas is displayed at.
 *
 * This looks like housekeeping and is not. A mismatch is *invisible* on
 * screen — the image is scaled into the box and looks right — and wrong for
 * anything that maps a screen coordinate back into the buffer, which is what
 * picking does. It went unnoticed until 2026-09-15 because the only thing that
 * maps one is a click, and the scene is a large centred plate that absorbed the
 * error: the canvas was 624 pixels tall inside a 468-pixel box on every run,
 * and a centre click was read a third of the height away from where it landed.
 *
 * What made it a *visible* failure was a seventh line of status text on the
 * worker-failure run, which took enough height out of the viewport to push the
 * click off the part. So a passing pick is not evidence this is right, and that
 * is exactly why it gets an assertion of its own rather than being left to the
 * pick to notice.
 */
function checkCanvasFit(r) {
  const f = r.canvasFit;
  check(
    'the canvas backing store is the size it is displayed at',
    f && f.backing[0] === f.css[0] && f.backing[1] === f.css[1],
    f ? `${f.backing.join('x')} backing, ${f.css.join('x')} displayed` : '(no canvas)',
  );
}

console.log('\n— WebGPU offered, cross-origin isolated —');
{
  const r = await run({ isolated: true, webgpu: true });
  check('the page starts', r.ready, r.error ?? '');
  if (r.ready) {
    check('an adapter answered', !!r.report.backend, `${r.report.backend} · ${r.report.adapter}`);
    check('frames were drawn', r.frames > 2, `${r.frames} frames`);
    // Deliberately not "and the backend is WebGPU". In this container it is
    // not: Chromium reports WebGPU, returns an adapter with a gigabyte of
    // buffer and compute shaders, and draws nothing. What must hold is that
    // the *page works anyway* and says why.
    check('the canvas is not blank', r.colours >= DRAWN, `${r.colours} distinct colours`);
    check(
      'a click in the middle names a body and a face',
      r.pick && r.pick.object !== null && r.pick.face !== null,
      JSON.stringify(r.pick),
    );
    check(
      'a fallback, if it happened, is stated',
      r.report.backend !== 'gl' || typeof r.graphics === 'string',
      r.graphics ?? '(no fallback)',
    );
    // Not "and it is hardware", which would be a claim about the container.
    // What must hold is that the answer is one of the three the enum has, and
    // that the note appears for exactly the one that means no GPU — a browser
    // that declines to say must not produce a warning, because on the web
    // that is most of them.
    check(
      'it says what is behind the adapter, or that nobody would say',
      ['hardware', 'software (CPU rasteriser)', 'unreported'].includes(r.report.acceleration) &&
        (r.report.acceleration === 'software (CPU rasteriser)') ===
          (typeof r.report.softwareRendering === 'string'),
      `${r.report.acceleration}${r.report.softwareRendering ? ` — ${r.report.softwareRendering}` : ''}`,
    );
    check('the page is cross-origin isolated', r.caps.isolated === true);
    checkVariant(r);
    // Not asserted against a threshold. It is one scene on one machine under a
    // software rasteriser, so a number here is a fact and not a target — but a
    // missing one would mean the timing never ran.
    check(
      'the tessellation was timed',
      typeof r.report.tessellateMs === 'number' && r.report.tessellateMs >= 0,
      `${r.report.tessellateMs} ms on a pool of ${r.report.threads}`,
    );
    // The page asked for a plate with a hole from the first day it drew
    // anything, and for nine days it got a plate: the backend's boolean
    // returned a copy of its first operand. Nothing in a screenshot or a
    // triangle count would say so, and this is the only check anywhere that
    // asserts the browser is modelling rather than displaying. Since the
    // worker became the boot path the answer is the *worker's* — this page has
    // no document — which is why `installWire` is made to carry it.
    check('the plate on screen was cut, not just asked for', r.report.modelled === true);
    checkCanvasFit(r);
    // Asserted on every run and not only on the one named for it: the worker
    // is the boot path everywhere, or it is a special case that happens to
    // hold in the test that looks for it.
    check('the scene was meshed off the thread that draws', r.report.meshedBy === 'worker',
      `meshed by ${r.report.meshedBy}${r.meshNote ? ` — ${r.meshNote}` : ''}`);
    check('nothing threw', r.consoleErrors.length === 0, r.consoleErrors.join(' | '));
  }
}

console.log('\n— WebGL2, the fallback —');
{
  const r = await run({ isolated: true, webgpu: false });
  check('the page starts without WebGPU', r.ready, r.error ?? '');
  if (r.ready) {
    // The whole point of the run. `gl` is wgpu's name for WebGL2 here.
    check('the backend really is WebGL2', r.report.backend === 'gl', r.report.backend);
    check(
      'and it reports no compute, rather than pretending',
      r.report.compute === false && typeof r.report.degradation === 'string',
      r.report.degradation ?? '(no degradation message)',
    );
    check('frames were drawn', r.frames > 2, `${r.frames} frames`);
    check('the canvas is not blank', r.colours >= DRAWN, `${r.colours} distinct colours`);
    // `Rg32Uint` as a render target and a scissored readback are the two
    // things most likely to be missing on WebGL2. This is the assertion the
    // whole file exists for.
    check(
      'ID-buffer picking works on WebGL2',
      r.pick && r.pick.object !== null && r.pick.face !== null,
      JSON.stringify(r.pick),
    );
    // The fallback path is the one that used to tessellate the scene a second
    // time, by calling `start` again — on exactly the machines least able to
    // afford it. It now re-uploads bytes it already has, so the mesh is still
    // the worker's and was made once.
    check(
      'and the WebGL2 fallback did not re-mesh on this thread',
      r.report.meshedBy === 'worker',
      `meshed by ${r.report.meshedBy}`,
    );
    // The fallback replaces the canvas element, so this is a second chance to
    // get its size wrong.
    checkCanvasFit(r);
    check('nothing threw', r.consoleErrors.length === 0, r.consoleErrors.join(' | '));
  }
}

console.log('\n— the worker boundary: the boot path —');
{
  const r = await run({ isolated: true, webgpu: true, compare: true });
  check('the page starts', r.ready, r.error ?? '');
  if (r.ready) {
    // The assertion this whole run exists for, and the one that is invisible
    // in a screenshot: the thread that draws did not mesh. A page that fell
    // back to `tessellateHere` draws the identical picture, in the identical
    // colours, with the identical triangle count — the only difference is the
    // stall, and nothing but this word reports it.
    check(
      'nothing was tessellated on the thread that draws',
      r.report.meshedBy === 'worker',
      `meshed by ${r.report.meshedBy}${r.meshNote ? ` — ${r.meshNote}` : ''}`,
    );
    check('and so the worker had to have run', r.wire !== null, r.meshNote ?? '');

    const w = r.wire;
    if (w) {
      check('it produced chunks', w.chunks > 0, `${w.chunks} chunks, ${w.bytes} bytes`);
      check(
        'every byte it sent arrived',
        w.bytes === w.claimed,
        `${w.bytes} arrived of ${w.claimed} sent`,
      );
      // The one that matters, and the one only the worker can answer: a
      // `postMessage` with a wrong transfer list still delivers, by copying,
      // and from this side the two are indistinguishable. The worker looks at
      // its own buffers afterwards — a transferred ArrayBuffer is detached.
      check(
        'the buffers were moved, not copied',
        w.transferred === true,
        w.transferred ? 'detached on the sending side' : 'still held by the worker',
      );
      // `modelled` has no field in WIRE.md — the format describes a mesh, not
      // a provenance — so it travels beside the bytes. Before the worker was
      // the boot path the page answered this from its own document; it no
      // longer has one, and a page that cannot say whether the plate was cut
      // is how a boolean that silently stopped working goes unnoticed for
      // nine days, which is what happened here once already.
      check('the plate on screen was cut, not just asked for', w.modelled === true);
      check(
        'the worker timed itself, phase by phase',
        ['modelMs', 'tessellateMs', 'encodeMs', 'initMs'].every(
          (k) => typeof w[k] === 'number' && w[k] >= 0,
        ),
        `instantiate ${Math.round(w.initMs)} · model ${Math.round(w.modelMs)} · ` +
          `mesh ${Math.round(w.tessellateMs)} · encode ${Math.round(w.encodeMs)} ms`,
      );
      // Encoding is a memcpy and a header per chunk against a tessellation
      // that is trigonometry. If this ever inverts, the format got expensive
      // and nothing else would say so.
      check(
        'encoding costs a fraction of meshing',
        w.encodeMs <= w.tessellateMs,
        `${Math.round(w.encodeMs)} ms encoding against ${Math.round(w.tessellateMs)} ms meshing`,
      );
    }

    // The reference, built on purpose now that the worker no longer has a
    // main-thread result to be compared against.
    const c = r.comparison;
    check('the main thread can still tessellate the same scene', c && !c.failed, c?.failed ?? '');
    if (c && !c.failed) {
      check(
        'and it agrees with the worker, triangle for triangle',
        c.after.triangles === c.before.triangles && c.after.triangles > 0,
        `${c.before.triangles} from the worker, ${c.after.triangles} from this thread`,
      );
      check(
        'the canvas still shows a solid',
        c.after.colours >= DRAWN,
        `${c.after.colours} distinct colours, against ${c.before.colours} before`,
      );
      // Installing bytes made here must *say* they were made here. If this
      // ever reads `worker`, `meshedBy` has stopped being evidence and the
      // check above it is worthless.
      check(
        'and installing them says so, rather than claiming a worker',
        c.before.meshedBy === 'worker' && c.after.meshedBy === 'main thread',
        `${c.before.meshedBy} → ${c.after.meshedBy}`,
      );
      check(
        'both routes agree the plate was cut',
        c.modelled === true && w?.modelled === true,
      );
      // Settles a question the code could only guess at: whether the
      // `Uint8Array`s `tessellateScene` returns are views into the module's
      // linear memory or copies on the JS heap. A backing buffer the size of
      // the chunk is a copy; a backing buffer of megabytes is the whole wasm
      // memory, and then every caller must copy them out before anything can
      // allocate. A fact, not a threshold — but a *change* here would change
      // what callers are obliged to do.
      check(
        'a chunk out of tessellateScene is backed by a buffer of its own size',
        c.chunkBuffer && c.chunkBuffer.buffer === c.chunkBuffer.chunk,
        c.chunkBuffer
          ? `${c.chunkBuffer.chunk} byte chunk in a ${c.chunkBuffer.buffer} byte buffer`
          : '(no chunks)',
      );
    }
    check('nothing threw', r.consoleErrors.length === 0, r.consoleErrors.join(' | '));
  }
}

console.log('\n— no COOP/COEP: the degradation must be visible —');
{
  const r = await run({ isolated: false, webgpu: true, coi: false });
  check('the page still starts', r.ready, r.error ?? '');
  if (r.ready) {
    check('the page is not isolated', r.caps.isolated === false);
    check('the single-threaded variant was chosen', r.chosen === 'single', r.chosen);
    check(
      'the reason names the headers',
      typeof r.note === 'string' && r.note.includes('Cross-Origin-Embedder-Policy'),
      r.note ?? '(no note)',
    );
    check(
      'and the user can see it',
      r.status.includes('Cross-Origin-Embedder-Policy'),
      r.status.split('\n').find((l) => l.includes('Cross-Origin-Embedder-Policy')) ?? '',
    );
    // Worth its own assertion because the two are easy to conflate: a *module
    // worker* needs no COOP/COEP at all. Only a **shared memory** does. So the
    // page that cannot have a thread pool still gets its tessellation off the
    // thread that draws, which is most of what the pool was wanted for.
    check(
      'but the tessellation is still off the thread that draws',
      r.report.meshedBy === 'worker',
      `meshed by ${r.report.meshedBy}${r.meshNote ? ` — ${r.meshNote}` : ''}`,
    );
  }
}

console.log('\n— the worker will not load: the page must still work —');
{
  const r = await run({ isolated: true, webgpu: true, breakWorker: true });
  // The whole point. A modeller that shows nothing because a worker would not
  // start is a worse answer than one that is slow, so the path `start` used to
  // take is still there and still correct.
  check('the page starts anyway', r.ready, r.error ?? '');
  if (r.ready) {
    check(
      'and it meshed on the thread that draws',
      r.report.meshedBy === 'main thread',
      `meshed by ${r.report.meshedBy}`,
    );
    check('with no worker result to show', r.wire === null);
    check('frames were drawn', r.frames > 2, `${r.frames} frames`);
    check('the canvas is not blank', r.colours >= DRAWN, `${r.colours} distinct colours`);
    // The same scene, by the path that does not cross a heap. If these ever
    // disagree with the worker's 6290, one of the two is wrong.
    check(
      'and it is the same scene the worker would have made',
      r.report.triangles > 0,
      `${r.report.triangles} triangles`,
    );
    // The run with the most status text, and so the smallest viewport — which
    // is what turned the stale backing store from a silent error into a missed
    // click in the first place.
    checkCanvasFit(r);
    check(
      'picking still works',
      r.pick && r.pick.object !== null && r.pick.face !== null,
      JSON.stringify(r.pick),
    );
    check('the plate was still cut', r.report.modelled === true);
    // Degrading quietly is the failure this repository keeps legislating
    // against. A page that silently meshes on the main thread is a stall
    // nobody can attribute six months later.
    check(
      'the reason is stated, not swallowed',
      typeof r.meshNote === 'string' && r.meshNote.includes('meshed'),
      r.meshNote ?? '(no note)',
    );
    check(
      'and the user can see it',
      r.status.includes('meshed on the thread that draws'),
      r.status.split('\n').find((l) => l.includes('thread that draws')) ?? '',
    );
  }
}

console.log('\n— no COOP/COEP, but a service worker: it must supply them —');
{
  // The GitHub Pages case. The first load is not isolated — a worker does not
  // control the navigation that registered it — so the page registers, reloads
  // itself, and comes back controlled. Everything asserted here is on the
  // second load, which is the one a returning visitor always gets.
  const r = await run({ isolated: false, webgpu: true, coi: true });
  check('the page starts', r.ready, r.error ?? '');
  if (r.ready) {
    check('the service worker made the page isolated', r.caps.isolated === true);
    check(
      'and the page says the headers came from it, not from the host',
      r.caps.isolatedBy === 'service worker',
      String(r.caps.isolatedBy),
    );
    check(
      'the user can see which',
      r.status.includes('isolated yes (service worker)'),
      r.status.split('\n').find((l) => l.includes('isolated')) ?? '',
    );
    // The point of the run that is easy to lose: what the worker bought. The
    // headers it supplies are only worth having if a threaded module can then
    // be given a shared memory — so this is where the service worker stops
    // being a page that says "isolated" and becomes a page that is faster for
    // it. Without the build it is the older assertion, that the platform is
    // ready and the artifact is not.
    checkVariant(r);
    check('nothing threw', r.consoleErrors.length === 0, r.consoleErrors.join(' | '));
  }
}

console.log('');
if (failures.length) {
  console.error(`${failures.length} failed: ${failures.join(', ')}`);
  process.exit(1);
}
console.log('all checks passed');
