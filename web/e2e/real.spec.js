// What only the real engine and viewer show (a release bundle from
// scripts/web/build.sh; skipped against the mock): which viewer build is
// fetched, the WebGL fallback, colour schemes baked by the worker, every
// example's preview and render with their timings, and the heavy example's
// long run, cancelled and completed. E2E_SHOTS=DIR saves each render and
// the timings (timings.json) there.

import { expect, test } from "@playwright/test";
import { writeFileSync } from "node:fs";
import { expectDrawn, open, shot, shots, summary, viewPixels } from "./helpers.js";

test.beforeEach(async ({ page }) => {
  await page.goto("/try/build.json");
  const build = JSON.parse(await page.locator("body").innerText());
  test.skip(build.engine !== "wasm" || build.view !== "wasm", "needs a release bundle with the wasm core and viewer");
});

/// The view's builds the page fetched.
function viewFetches(page) {
  const got = [];
  page.on("response", (r) => {
    const m = r.url().match(/\/(view(?:-webgl)?)\/web_view_bg\.wasm$/);
    if (m) got.push(m[1]);
  });
  return got;
}

test("WebGPU where the browser has it, without fetching the WebGL build", async ({ page }) => {
  const fetched = viewFetches(page);
  const { failed, errors } = await open(page);
  await expect(summary(page)).toContainText("Previewed");
  const gpu = await page.evaluate(() => "gpu" in navigator);
  const kind = await page.evaluate(() => document.documentElement.dataset.view);
  console.log(`navigator.gpu: ${gpu}; view: ${kind} (${await page.evaluate(() => window.NeoSCADWeb.viewer.adapter)})`);
  if (gpu && kind === "webgpu") expect(fetched).toEqual(["view"]);
  else expect(fetched).toContain("view-webgl");
  expect(["webgpu", "webgl"]).toContain(kind);
  await expect(page.getByTestId("view-notice")).toBeHidden();
  await expectDrawn(page);
  expect(failed).toEqual([]);
  expect(errors).toEqual([]);
  await shot(page, "real-csg");
});

test("without navigator.gpu the WebGL build is loaded lazily and draws", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 1400, height: 860 } });
  await context.addInitScript(() => {
    delete Navigator.prototype.gpu;
  });
  const page = await context.newPage();
  const fetched = viewFetches(page);
  await open(page);
  expect(await page.evaluate(() => "gpu" in navigator)).toBe(false);
  await expect(summary(page)).toContainText("Previewed");
  expect(await page.evaluate(() => document.documentElement.dataset.view)).toBe("webgl");
  expect(fetched).toEqual(["view-webgl"]);
  await expect(page.getByTestId("view-notice")).toBeHidden();
  await expectDrawn(page);
  await shot(page, "real-webgl-csg");
  await context.close();
});

test("with neither WebGPU nor WebGL the canvas fallback draws, with its notice", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 1400, height: 860 } });
  await context.addInitScript(() => {
    delete Navigator.prototype.gpu;
    const get = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (type, ...rest) {
      return /webgl|webgpu/.test(type) ? null : get.call(this, type, ...rest);
    };
  });
  const page = await context.newPage();
  await open(page, "#example=sign");
  await expect(summary(page)).toContainText("Previewed");
  expect(await page.evaluate(() => document.documentElement.dataset.view)).toBe("canvas2d");
  await expect(page.getByTestId("view-notice")).toBeVisible();
  await expectDrawn(page);
  await shot(page, "real-canvas2d-sign");
  await context.close();
});

test("a colour scheme is baked by the worker: changing it runs again", async ({ page }) => {
  await open(page);
  await expect(summary(page)).toContainText("Previewed");
  const before = await page.evaluate(() => window.NeoSCADWeb.engine.nextId);
  const runs = [];
  await page.exposeFunction("__sawRun", (s) => runs.push(s));
  await page.evaluate(() => {
    const w = window.NeoSCADWeb.engine.worker;
    const post = w.postMessage.bind(w);
    w.postMessage = (m, t) => {
      if (m.type === "run") window.__sawRun(m.colorScheme ?? "");
      return post(m, t);
    };
  });
  const bg = (await viewPixels(page)).top;
  await page.getByTestId("view-menu").click();
  await page.getByRole("menuitemcheckbox", { name: "Tomorrow Night", exact: true }).click();
  await expect.poll(() => runs).toContain("Tomorrow Night");
  await expect(summary(page)).toContainText("Previewed");
  expect(await page.evaluate(() => window.NeoSCADWeb.engine.nextId)).toBeGreaterThan(before);
  await expectDrawn(page);
  expect((await viewPixels(page)).top).not.toBe(bg);
  await shot(page, "real-csg-tomorrow-night");
  // Presets, toggles and View All go to the viewer.
  await page.getByTestId("view-menu").click();
  await page.getByRole("menuitem", { name: "Top", exact: true }).click();
  const cam = await page.evaluate(() => window.NeoSCADWeb.viewer.camera());
  expect(cam.vpr.map(Math.round)).toEqual([0, 0, 0]);
  await page.getByTestId("view-menu").click();
  await page.getByRole("menuitemcheckbox", { name: "Show Edges" }).click();
  expect(await page.evaluate(() => window.NeoSCADWeb.viewer.raw.settings().edges)).toBe(true);
});

test("a drag orbits the view", async ({ page }) => {
  await open(page);
  await expect(summary(page)).toContainText("Previewed");
  const before = await page.evaluate(() => window.NeoSCADWeb.viewer.camera().vpr);
  const box = await page.locator("#viewport").boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 80, box.y + box.height / 2 + 30, { steps: 8 });
  await page.mouse.up();
  const after = await page.evaluate(() => window.NeoSCADWeb.viewer.camera().vpr);
  expect(after).not.toEqual(before);
  await page.mouse.wheel(0, -300);
});

test("measure picks a point on the model through the viewer's ray", async ({ page }) => {
  await open(page, "#example=box-lid");
  await page.getByRole("tab", { name: "Measure" }).click();
  const panel = page.getByTestId("measure");
  await panel.getByRole("button", { name: "Measure", exact: true }).click();
  await expect(panel.getByRole("heading", { name: "Parts", exact: true })).toBeVisible();
  await panel.getByRole("checkbox", { name: /Pick/ }).check();
  const box = await page.locator("#viewport").boundingBox();
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  await expect.poll(() => page.evaluate(() => window.NeoSCADWeb.measure.picks.length)).toBe(1);
  await shot(page, "real-measure-pick");
});

test("the language server answers from the worker: go to a BOSL2 definition", async ({ page }) => {
  await open(page, "#example=gear");
  await expect(summary(page)).toContainText("Previewed");
  expect(await page.evaluate(() => window.NeoSCADEditor.lspReady())).toBe(true);
  const at = (await page.evaluate(() => window.NeoSCADEditor.text().text)).indexOf("\nspur_gear(") + 3;
  await page.evaluate((pos) => window.NeoSCADEditor.definition(pos), at);
  // The definition is in BOSL2's gears.scad, read back from the worker
  // (`readFile`) into a read-only tab.
  await expect(page.locator("#editor-tabs [role=tab]").nth(1)).toContainText("gears.scad");
  expect(await page.evaluate(() => window.NeoSCADEditor.text().text)).toContain("module spur_gear(");
  await shot(page, "real-gear-definition");
});

const timings = [];

test("every example previews and renders; timings", async ({ page }) => {
  test.setTimeout(300000);
  await open(page);
  const examples = await page.evaluate(() => window.NeoSCADWeb.manifest.examples.filter((e) => !e.heavy));
  for (const e of examples) {
    await page.goto(`/try/#example=${e.id}`);
    await page.reload();
    await page.waitForSelector("html[data-ready]");
    const row = { id: e.id };
    for (const mode of ["preview", "render"]) {
      if (mode === "render" || !e.autorun) await page.getByTestId(mode).click();
      const t0 = Date.now();
      await expect(summary(page)).toContainText(mode === "preview" ? "Previewed" : "Rendered", { timeout: 120000 });
      const run = await page.evaluate(() => window.NeoSCADWeb.lastRun);
      expect(run.exitCode).toBe(0);
      row[mode] = { totalMs: Math.round(run.timings.totalMs), waitedMs: Date.now() - t0 };
      await expectDrawn(page);
      await shot(page, `real-${e.id}-${mode}`);
    }
    timings.push(row);
    console.log(JSON.stringify(row));
  }
});

test("the heavy example shows its long run, cancels, and completes", async ({ page }) => {
  test.setTimeout(300000);
  const fetched = [];
  page.on("response", (r) => r.url().endsWith("/bosl2.tar.gz") && fetched.push(r.status()));
  await open(page, "#example=gearbox");
  await expect(summary(page)).toContainText("Heavy");
  // Start, see it running, cancel: the worker is terminated and a new
  // one takes the document.
  await page.getByTestId("preview").click();
  await expect(page.getByTestId("engine-status")).toHaveText(/working/, { timeout: 15000 });
  await expect(page.getByRole("button", { name: "Cancel" })).toBeVisible();
  await page.getByRole("button", { name: "Cancel" }).click();
  await expect(summary(page)).toContainText("Cancelled; the engine restarted.");
  await expect(page.getByTestId("engine-status")).toHaveText("engine restarted");
  // Then let it finish: BOSL2 comes back from the page's copy, not the
  // network.
  const row = { id: "gearbox" };
  for (const mode of ["preview", "render"]) {
    const t0 = Date.now();
    await page.getByTestId(mode).click();
    await expect(summary(page)).toContainText(mode === "preview" ? "Previewed" : "Rendered", { timeout: 240000 });
    const run = await page.evaluate(() => window.NeoSCADWeb.lastRun);
    expect(run.exitCode).toBe(0);
    row[mode] = { totalMs: Math.round(run.timings.totalMs), waitedMs: Date.now() - t0 };
    await expectDrawn(page);
    await shot(page, `real-gearbox-${mode}`);
  }
  row.memoryBytes = (await page.evaluate(() => window.NeoSCADWeb.engine.request({ type: "stats" }))).memoryBytes;
  expect(fetched).toEqual([200]);
  timings.push(row);
  console.log(JSON.stringify(row));
});

test.afterAll(() => {
  if (shots && timings.length) writeFileSync(`${shots}/timings.json`, JSON.stringify(timings, null, 2) + "\n");
});
