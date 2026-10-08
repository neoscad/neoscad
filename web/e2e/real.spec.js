// What only the real engine and viewer show (a release bundle from
// scripts/web/build.sh; skipped against the mock): which viewer build is
// fetched, the WebGL fallback, colour schemes baked by the worker, every
// example's preview and render with their timings, and the heavy example's
// long run, cancelled and completed. E2E_SHOTS=DIR saves each render and
// the timings (timings.json) there.

import { expect, test } from "@playwright/test";
import { writeFileSync } from "node:fs";
import { expectDrawn, open, pngPixels, shot, shots, summary, viewPixels } from "./helpers.js";

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

// Dragging the editor's splitter resizes the view every frame. A resize
// reconfigures the surface, which clears the canvas; the viewer used to
// draw again only at the next animation frame, after the cleared canvas had
// been painted, so for as long as the drag lasted every frame on screen was
// blank. Screenshots taken while the width is still changing catch it (one
// taken after the drag would not: by then the late frame has drawn).
for (const backend of ["webgpu", "webgl"]) {
  test(`the view stays drawn while the editor's splitter is dragged (${backend})`, async ({ browser }) => {
    const context = await browser.newContext({ viewport: { width: 1400, height: 860 } });
    if (backend === "webgl") {
      await context.addInitScript(() => {
        delete Navigator.prototype.gpu;
      });
    }
    const page = await context.newPage();
    const { errors } = await open(page);
    await expect(summary(page)).toContainText("Previewed");
    const kind = await page.evaluate(() => document.documentElement.dataset.view);
    test.skip(kind !== backend, `this browser's view is ${kind}`);
    await expectDrawn(page);

    // A real press on the splitter (it captures the pointer), then a move
    // every animation frame, as a hand's drag delivers them, between 25%
    // and 45% of the workspace.
    const split = await page.locator("#split-left").boundingBox();
    await page.mouse.move(split.x + split.width / 2, split.y + split.height / 2);
    await page.mouse.down();
    await page.evaluate(() => {
      const el = document.querySelector("#split-left");
      const w = document.querySelector("#workspace").getBoundingClientRect();
      let i = 0;
      window.__dragging = true;
      const step = () => {
        if (!window.__dragging) return;
        i += 1;
        const at = w.left + w.width * (0.25 + 0.2 * Math.abs(((i % 40) - 20) / 20));
        el.dispatchEvent(new PointerEvent("pointermove", { clientX: at, clientY: w.top + 100, bubbles: true }));
        requestAnimationFrame(step);
      };
      requestAnimationFrame(step);
    });
    // The part of the view that the left pane never covers in that range.
    const ws = await page.locator("#workspace").boundingBox();
    const inspector = await page.locator("#split-right").boundingBox();
    const x = Math.ceil(ws.x + ws.width * 0.46);
    const clip = { x, y: ws.y + 40, width: Math.floor(inspector.x) - x, height: ws.height - 80 };
    const during = [];
    for (let k = 0; k < 4; k++) during.push(await pngPixels(page, await page.screenshot({ clip })));
    await page.evaluate(() => {
      window.__dragging = false;
    });
    await page.mouse.up();
    // A blank canvas is one colour (the page behind it); a drawn one has
    // the model's shading, the grid and the axes.
    expect(during.map((p) => p.distinct > 12)).toEqual([true, true, true, true]);
    await expectDrawn(page);
    expect(errors).toEqual([]);
    await context.close();
  });
}

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

// Deep recursion in every engine, through the page and a cold worker (a
// fresh page load each time): WebKit's worker has about 512 KiB of stack
// and JavaScriptCore's baseline tier spends about a kilobyte a wasm frame,
// so recursion that used native frames overflowed there at a few dozen
// levels and trapped the instance. The evaluator's heap stack makes depth
// a count: 99,999 levels run, and the 100,000th is OpenSCAD's recursion
// error, with the worker still alive. crates/web/test/run.mjs checks the
// same in node (V8); this is the check that runs in JavaScriptCore and
// SpiderMonkey.
const DEEP = {
  function: (n) => `function f(n) = n == 0 ? 0 : 1 + f(n - 1);\necho(f(${n}));\n`,
  module: (n) => `module m(n) { if (n > 0) m(n - 1); else cube(1); }\nm(${n});\n`,
  comprehension: (n) => `function g(n) = n == 0 ? [] : [for (i = [0:0]) each g(n - 1)];\necho(len(g(${n})));\n`,
  children: (n) => `module c(n) { if (n > 0) c(n - 1) children(); else children(); }\nc(${n}) cube(1);\n`,
};
const codeLink = (text) => `#code=${Buffer.from(text, "utf8").toString("base64url")}&name=deep`;

test("deep recursion reaches the counted limit, and past it is an error, not a crash", async ({ page }) => {
  test.setTimeout(120000);
  for (const [kind, source] of Object.entries(DEEP)) {
    for (const [n, stops] of [[99999, false], [100000, true]]) {
      await page.goto("about:blank");
      await open(page, codeLink(source(n)));
      await expect.poll(() => page.evaluate(() => window.NeoSCADWeb.lastRun?.exitCode), { timeout: 60000 }).not.toBeUndefined();
      const text = (await page.locator(".console-line").allInnerTexts()).join("\n");
      expect(await page.evaluate(() => window.NeoSCADWeb.engine.restarts), `${kind} at ${n}`).toBe(0);
      if (stops) expect(text, `${kind} at ${n}`).toMatch(/Recursion detected calling (function|module) '[fgmc]'/);
      else {
        expect(text, `${kind} at ${n}`).not.toMatch(/Recursion detected/);
        expect(await page.evaluate(() => window.NeoSCADWeb.lastRun.exitCode), `${kind} at ${n}`).toBe(0);
        if (kind === "function") expect(text).toContain("ECHO: 99999");
      }
    }
  }
});

// View > "Edge fillets and chamfers (fillet)" sends `fillet` with every
// run: off, the call is OpenSCAD's unknown module; on, the engine knows
// it and the warning goes (docs/fillet-edges.md).
test("the fillet setting reaches the engine's runs", async ({ page }) => {
  await open(page, codeLink('fillet_edges(r = 1, edges = "|z") cube(10);\n'));
  await expect.poll(() => page.evaluate(() => window.NeoSCADWeb.lastRun?.exitCode), { timeout: 60000 }).not.toBeUndefined();
  const lines = async () => (await page.locator(".console-line").allInnerTexts()).join("\n");
  expect(await lines()).toContain("Ignoring unknown module 'fillet_edges'");
  await page.getByTestId("view-menu").click();
  await page.getByRole("menuitemcheckbox", { name: "Edge fillets and chamfers (fillet)" }).click();
  await expect.poll(lines, { timeout: 60000 }).not.toContain("unknown module");
  await expect(summary(page)).toContainText("Previewed");
});

// The fonts are not in the core: fonts.tar.gz is fetched the first time a
// model draws text, either because the page sees `text(` in it or because
// the core says a run wanted fonts (text drawn inside a library), and
// never for a model without text.
test("the fonts are fetched once, only when a model draws text", async ({ page }) => {
  const fetched = [];
  page.on("response", (r) => r.url().endsWith("/fonts.tar.gz") && fetched.push(r.status()));
  await open(page, "#example=csg");
  await expect(summary(page)).toContainText("Previewed");
  expect(fetched).toEqual([]);
  // BOSL2's text3d() calls text() where the page cannot see it.
  await page.goto("about:blank");
  await open(page, codeLink('include <BOSL2/std.scad>\ntext3d("NeoSCAD", h = 3);\n'));
  await expect.poll(() => page.evaluate(() => window.NeoSCADWeb.lastRun?.exitCode), { timeout: 60000 }).toBe(0);
  expect(fetched).toEqual([200]);
  expect((await page.locator(".console-line").allInnerTexts()).join("\n")).not.toMatch(/Can't get font/);
  await expectDrawn(page);
  // The sign example's text is seen in its source: fetched before its run.
  await page.goto("about:blank");
  fetched.length = 0;
  await open(page, "#example=sign");
  await expect(summary(page)).toContainText("Previewed");
  expect(fetched).toEqual([200]);
  expect((await page.locator(".console-line").allInnerTexts()).join("\n")).not.toMatch(/Can't get font/);
  await page.getByTestId("preview").click();
  await expect(summary(page)).toContainText("Previewed");
  expect(fetched).toEqual([200]);
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
