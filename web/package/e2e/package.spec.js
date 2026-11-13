// The built package in a browser, as a third-party page uses it: render()
// self-hosted and from another origin (the blob: worker), the custom
// element ten at a time, export, abort, and an app that bundled it.
//
// Against the mock build (CI) the engine is the demo's mock core and the
// viewer the canvas2d fallback (the mock build ships no viewer); with
// E2E_PACKAGE_REAL=1 against a real build, the wasm core and the WebGPU or
// WebGL viewer.

import { expect, test } from "@playwright/test";

const REAL = !!process.env.E2E_PACKAGE_REAL;
const CDN = Number(process.env.E2E_CDN_PORT ?? 8125);

async function open(page, path) {
  const problems = [];
  page.on("pageerror", (e) => problems.push(String(e)));
  // Why a viewer fell back (view/index.js logs each failed backend).
  page.on("console", (m) => m.type() === "warning" && /3D view/.test(m.text()) && console.log(`  page: ${m.text().slice(0, 200)}`));
  await page.goto(path);
  await page.waitForFunction(() => window.NS_READY === true);
  return problems;
}

/// How many pixels of the viewer's image differ from its corner (the
/// background): a drawn model covers a good part of the view.
async function drawnPixels(page, expr) {
  return page.evaluate(async (expr) => {
    const blob = await eval(expr);
    const bitmap = await createImageBitmap(blob);
    const c = document.createElement("canvas");
    c.width = bitmap.width;
    c.height = bitmap.height;
    const ctx = c.getContext("2d");
    ctx.drawImage(bitmap, 0, 0);
    const d = ctx.getImageData(0, 0, c.width, c.height).data;
    const bg = [d[0], d[1], d[2]];
    let n = 0;
    for (let i = 0; i < d.length; i += 4) if (Math.abs(d[i] - bg[0]) + Math.abs(d[i + 1] - bg[1]) + Math.abs(d[i + 2] - bg[2]) > 30) n++;
    return { n, total: d.length / 4, type: blob.type };
  }, expr);
}

for (const from of ["same", "cdn"]) {
  test(`render() draws a cube (${from === "cdn" ? "from another origin, blob: worker" : "self-hosted"})`, async ({ page }) => {
    const problems = await open(page, `/index.html?from=${from}&cdn=${CDN}`);
    const r = await page.evaluate(async () => {
      const view = await NS.render(document.getElementById("view"), "cube(10, center = true);\necho(\"hi\");");
      window.view = view;
      return {
        ok: view.result.ok,
        bbox: view.result.bbox,
        echo: view.result.console.find((l) => l.kind === "echo")?.text,
        kind: view.viewer.kind,
        viaBlob: view.engine.spawner.viaBlob,
        version: view.engine.version,
      };
    });
    console.log(`  viewer: ${r.kind}; engine ${r.version}; blob worker: ${r.viaBlob}`);
    expect(r.ok).toBe(true);
    expect(r.viaBlob).toBe(from === "cdn");
    expect(r.echo).toContain("hi");
    if (REAL) {
      expect(r.bbox).toEqual({ min: [-5, -5, -5], max: [5, 5, 5] });
      expect(["webgpu", "webgl"]).toContain(r.kind);
    } else {
      expect(r.version).toBe("0.0.0-mock");
      expect(r.kind).toBe("canvas2d");
    }
    const px = await drawnPixels(page, "view.viewer.image(200, 150)");
    expect(px.type).toBe("image/png");
    expect(px.total).toBe(200 * 150);
    expect(px.n).toBeGreaterThan(px.total * 0.03);
    expect(problems).toEqual([]);
  });
}

test("update() re-runs; errors come back with positions; export gives a file", async ({ page }) => {
  await open(page, "/index.html");
  const r = await page.evaluate(async (real) => {
    const view = await NS.render(document.getElementById("view"), "cube(1);");
    // A warning from evaluation, then (real engine only) a syntax error,
    // which stops the parse before any evaluation warns.
    const warned = await view.update("cube(2);\nsphere(r = nope);");
    const bad = real ? await view.update("cube(2);\n// é\ncube(;") : { ok: false, errors: [] };
    const good = await view.update("cube([1, 2, 3]);");
    const exp = await view.export(real ? "binstl" : "stl");
    const head = new TextDecoder().decode(exp.data.slice(0, 5));
    const count = exp.data.length >= 84 ? new DataView(exp.data.buffer, exp.data.byteOffset).getUint32(80, true) : -1;
    let refused = null;
    try {
      await view.engine.export("cube(1);", "step");
    } catch (e) {
      refused = { name: e.name, failure: e.failure };
    }
    return { warned: { ok: warned.ok, warnings: warned.warnings }, bad: { ok: bad.ok, errors: bad.errors }, good: good.ok, length: exp.data.length, head, count, ok: exp.ok, refused };
  }, REAL);
  expect(r.good).toBe(true);
  expect(r.warned.ok).toBe(true);
  expect(r.warned.warnings[0]).toMatchObject({ path: "main.scad", line: 1 });
  expect(r.warned.warnings[0].message).toContain("nope");
  if (REAL) {
    expect(r.bad.ok).toBe(false);
    expect(r.bad.errors[0]).toMatchObject({ code: "syntax-error", path: "main.scad", line: 2, column: 5 });
    // A binary STL: 80-byte header, a count, 50 bytes a triangle.
    expect(r.length).toBe(84 + 50 * r.count);
    expect(r.count).toBe(12);
  } else {
    expect(r.head).toBe("solid");
  }
  expect(r.ok).toBe(true);
  expect(r.refused).toMatchObject({ name: "ExportError" });
  expect(r.refused.failure).toContain("exact");
});

test("aborting one model's run leaves another model's runs alone", async ({ page }) => {
  test.skip(REAL, "uses the mock's mock:slow");
  await open(page, "/index.html");
  const r = await page.evaluate(async () => {
    const engine = NS.sharedEngine();
    const slow = engine.model();
    const other = engine.model();
    const ac = new AbortController();
    const a = slow.run("// mock:slow=3000\ncube(1);", { signal: ac.signal }).then(() => "finished", (e) => e.name);
    const b = other.run("cube(2);");
    setTimeout(() => ac.abort(), 300);
    const t0 = performance.now();
    const [ra, rb] = await Promise.all([a, b]);
    return { ra, ok: rb.ok, ms: performance.now() - t0 };
  });
  expect(r.ra).toBe("AbortError");
  expect(r.ok).toBe(true);
  expect(r.ms).toBeLessThan(2500);
});

test("ten elements share one engine; an error shows; an off-screen one waits until scrolled to", async ({ page }) => {
  const problems = await open(page, `/elements.html?from=cdn&cdn=${CDN}`);
  const ids = [...Array(8).keys()].map((i) => `m${i}`).concat("fromsrc");
  await page
    .waitForFunction((ids) => ids.every((id) => id in window.results), ids, { timeout: REAL ? 80000 : 20000 })
    .catch(async (e) => {
      console.log(await page.evaluate(() => JSON.stringify({ results: window.results, errors: window.errors })));
      throw e;
    });
  const results = await page.evaluate(() => window.results);
  for (const id of ids) expect(results[id], id).toBe(true);
  await page.waitForFunction(() => "broken" in window.errors);
  const status = page.locator("#broken").locator("[part=status]");
  await expect(status).toBeVisible();
  await expect(status).toHaveAttribute("data-kind", "failed");
  if (REAL) await expect(status).toContainText("syntax error");
  expect(await page.evaluate(() => "far" in window.results)).toBe(false);
  // Each viewer is made as its element comes into view, apart from its run.
  await page.waitForFunction((ids) => ids.every((id) => document.getElementById(id).dataset.backend), ids, { timeout: 20000 });
  const kinds = await page.evaluate((ids) => ids.map((id) => document.getElementById(id).dataset.backend), ids);
  console.log(`  backends: ${[...new Set(kinds)].join(", ")}`);
  expect(kinds.every((k) => k)).toBe(true);
  const engines = await page.evaluate((ids) => new Set(ids.map((id) => document.getElementById(id).engine)).size, ids);
  expect(engines).toBe(1);
  await page.locator("#far").scrollIntoViewIfNeeded();
  await page.waitForFunction(() => window.results.far === true);
  // Scrolled away for good, the first elements give their viewers back.
  await page.waitForFunction(() => !document.getElementById("m0").viewer, null, { timeout: 15000 });
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.waitForFunction(() => !!document.getElementById("m0").viewer);
  expect(problems).toEqual([]);
});

test("attributes re-run or re-draw; the source property replaces the inline script", async ({ page }) => {
  await open(page, "/elements.html");
  await page.waitForFunction(() => window.results.m0 === true);
  const r = await page.evaluate(async () => {
    const el = document.getElementById("m0");
    const runs = [];
    el.addEventListener("neoscad-result", (e) => runs.push(e.detail.ok));
    el.setAttribute("axes", "false");
    await new Promise((r) => setTimeout(r, 200));
    const afterView = runs.length;
    el.source = "cylinder(h = 3, r = 2);";
    await new Promise((resolve) => el.addEventListener("neoscad-result", resolve, { once: true }));
    return { afterView, runs: runs.length, bbox: el.result.bbox };
  });
  expect(r.afterView).toBe(0);
  expect(r.runs).toBe(1);
  if (REAL) expect(r.bbox.max[2]).toBeCloseTo(3);
});

test("an app that bundled the package and passes assetsUrl", async ({ page }) => {
  const problems = [];
  page.on("pageerror", (e) => problems.push(String(e)));
  await page.goto("/app/");
  await page.waitForFunction(() => !!window.APP, null, { timeout: REAL ? 60000 : 15000 });
  const app = await page.evaluate(() => window.APP);
  expect(app.ok).toBe(true);
  expect(app.assets).toMatch(/\/pkg\/dist\/assets\/$/);
  expect(app.version).not.toBe("dev");
  if (REAL) expect(app.bbox).toEqual({ min: [0, 0, 0], max: [4, 6, 8] });
  expect(problems).toEqual([]);
});
