// The only check in this repository that runs the viewport in a browser.
//
// Everything else is native, and native proves the pipeline but not the
// platform: WebGL2 in particular has no compute shaders, a different set of
// renderable formats and its own idea of what a downlevel limit is. Until this
// file existed, every claim in STACK.md about the fallback was an argument.
//
// It runs the page four times, and the four are different questions:
//
//   1. WebGPU offered — whatever the page ends up on, it must draw and pick.
//      Headless Chromium here reports WebGPU and then rasterises nothing, so
//      this is also the test of the loader's fall back *from evidence*.
//   2. WebGL2, by launching a browser with no `navigator.gpu` at all, so the
//      fallback is exercised rather than described.
//   3. No COOP/COEP and no service worker — the loader must degrade *visibly*,
//      which is a rule in STACK.md and otherwise nothing checks it.
//   4. No COOP/COEP, service worker allowed — the case every GitHub Pages
//      visitor is in. `coi-serviceworker.js` must supply the headers the host
//      never sends, and the page must end up isolated and say where the
//      isolation came from. Without this run the worker is a claim.
//
// Runs 1 and 4 also decide the variant, and what they assert depends on
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
async function run({ isolated = true, webgpu = true, coi = true } = {}) {
  const { proc, url } = await serve({ isolated });
  // `?coi=off` is the page's own hook for skipping service-worker registration.
  // Run 3 needs it: with the worker in play a host that sends no headers is no
  // longer a page that cannot be isolated, and the degradation it is there to
  // check never happens.
  const target = coi ? url : `${url}?coi=off`;
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
        status: document.getElementById('status')?.textContent ?? '',
      };
    });

    let pick = null;
    let colours = 0;
    if (state.ready) {
      // Let the loop run so `frames` is a rendered frame count, not zero.
      await page.waitForFunction(() => globalThis.__w3d.frames > 2, null, { timeout: 10000 });
      // Asked of the page, not taken from a screenshot. Two reasons: a PNG's
      // byte count is a poor proxy — a blank 960x660 frame still encodes to a
      // few kilobytes, which is how the first version of this file reported a
      // black canvas as drawn — and a WebGL2 drawing buffer is empty by the
      // time anything outside the rendering task looks at it.
      colours = await page.evaluate(() => globalThis.__w3d.sample());

      const box = await page.locator('#viewport').boundingBox();
      await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
      pick = await page
        .waitForFunction(() => globalThis.__w3d.lastPick ?? null, null, { timeout: 10000 })
        .then((h) => h.jsonValue())
        .catch(() => null);
      state.frames = await page.evaluate(() => globalThis.__w3d.frames);
    }

    return { ...state, pick, colours, consoleErrors };
  } finally {
    await browser.close();
    proc.kill();
  }
}

/** A lit solid on a dark background is many colours. One is a blank canvas. */
const DRAWN = 8;

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
    // asserts the browser is modelling rather than displaying.
    check('the plate on screen was cut, not just asked for', r.report.modelled === true);
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
