// The page in a browser, served under /try/ with whichever engine the
// build has: the wasm core in a release bundle (scripts/web/build.sh), the
// mock in `npm run build`. Checks the layout, the site contract, examples,
// the editor loop, the console, the customizer and its persistence,
// downloads, F5, the check and measure panels, and a respawn after a
// crash. real.spec.js adds what only the wasm core and viewer can show.
// Set E2E_SHOTS=DIR to save screenshots there.

import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { editorText, engineKind, expectDrawn, open, settleCursor, shot, stlTriangles, summary } from "./helpers.js";

test("the layout, the site contract and a first preview", async ({ page }) => {
  const { failed, errors } = await open(page);
  await expect(summary(page)).toContainText(/Previewed|Rendered/);
  // Editor over console on the left, view in the middle, inspector right.
  const box = async (sel) => page.locator(sel).boundingBox();
  const editor = await box("#editor");
  const consoleBox = await box("#console");
  const view = await box("#viewport");
  const inspector = await box("#inspector");
  expect(editor.y).toBeLessThan(consoleBox.y);
  expect(editor.x + editor.width).toBeLessThanOrEqual(view.x + 1);
  expect(view.x + view.width).toBeLessThanOrEqual(inspector.x + 1);
  // The site's nav and theme were applied: serve.mjs's stand-ins, or with
  // E2E_SITE the website's own (whose nav ends in "Try it" too).
  await expect(page.locator("#site-nav a").last()).toHaveText("Try it");
  await expect(page.locator("#site-nav a[aria-current=page]")).toHaveText("Try it");
  if (!process.env.E2E_SITE && !process.env.E2E_URL) {
    await expect(page.locator("#site-nav a")).toHaveText(["Home", "Try it"]);
    const accent = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--ns-accent").trim());
    expect(accent).toBe("rgb(1, 2, 3)");
  }
  // A drawn view (sampled from the screen, whichever backend drew it).
  await expectDrawn(page);
  expect(failed).toEqual([]);
  expect(errors).toEqual([]);
  await shot(page, "desktop-csg");
});

test("examples switch, and edits persist until Reset example", async ({ page }) => {
  await open(page);
  const picker = page.getByTestId("example-picker");
  await expect(picker.locator("option")).toHaveCount(8);
  await expect(picker.locator("option", { hasText: "(heavy)" })).toHaveCount(1);
  await picker.selectOption("menger");
  await expect(page.locator("#editor-tabs [role=tab]").first()).toHaveText("example024.scad");
  expect(await editorText(page)).toContain("Menger Sponge");
  // Type at the start of the document.
  await page.locator(".cm-content").click();
  await page.keyboard.press("ControlOrMeta+Home");
  await settleCursor(page, 0);
  await page.keyboard.type("// edited\n");
  await expect.poll(() => editorText(page)).toMatch(/^\/\/ edited\n/);
  await page.waitForTimeout(400);
  await page.reload();
  await page.waitForSelector("html[data-ready]");
  await expect(picker).toHaveValue("menger");
  expect(await editorText(page)).toMatch(/^\/\/ edited\n/);
  await page.getByTestId("reset-example").click();
  await expect.poll(() => editorText(page)).toMatch(/^\/\/ Menger Sponge/);
});

test("the customizer builds controls from the parameters and keeps values", async ({ page }) => {
  await open(page, "#example=sign");
  const panel = page.getByTestId("customizer");
  await expect(panel.locator("summary")).toHaveText(["properties of Sign", "Content To be written"]);
  await expect(panel.locator("#param-resolution")).toHaveValue("0");
  const radius = panel.locator('.param[data-name="radius"] input.number');
  await expect(radius).toHaveValue("80");
  await radius.fill("150");
  await radius.press("Enter");
  await expect(panel.locator('.param[data-name="radius"]')).toHaveClass(/is-set/);
  await panel.locator("#param-Message").selectOption({ label: "Thank You" });
  await expect(summary(page)).toContainText("Previewed");
  await shot(page, "desktop-sign-customizer");
  await page.reload();
  await page.waitForSelector("html[data-ready]");
  await expect(panel.locator('.param[data-name="radius"] input.number')).toHaveValue("150");
  await expect(panel.locator("#param-Message option:checked")).toHaveText("Thank You");
  // Per-parameter reset, then Reset for the rest.
  await panel.getByRole("button", { name: "Reset radius" }).click();
  await expect(panel.locator('.param[data-name="radius"] input.number')).toHaveValue("80");
  await panel.getByRole("button", { name: "Reset", exact: true }).click();
  await expect(panel.locator(".param.is-set")).toHaveCount(0);
});

test("Export downloads a file named after the example", async ({ page }) => {
  await open(page);
  await expect(summary(page)).toContainText("Previewed");
  await page.getByTestId("export-menu").click();
  const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("menuitem", { name: "STL…" }).click()]);
  expect(download.suggestedFilename()).toBe("CSG.stl");
  const triangles = stlTriangles(readFileSync(await download.path()));
  expect(triangles).toBeGreaterThan(10);
  console.log(`CSG.stl: ${triangles} triangles`);
  await page.getByTestId("export-menu").click();
  const [threemf] = await Promise.all([page.waitForEvent("download"), page.getByRole("menuitem", { name: "3MF…" }).click()]);
  expect(threemf.suggestedFilename()).toBe("CSG.3mf");
  expect([...readFileSync(await threemf.path()).subarray(0, 2)]).toEqual([0x50, 0x4b]);
});

test("STEP export is behind its toggle and reports how much is exact", async ({ page }) => {
  await open(page);
  await expect(summary(page)).toContainText("Previewed");
  await page.getByTestId("export-menu").click();
  await expect(page.getByRole("menuitem", { name: "STEP (exact surfaces)…" })).toHaveCount(0);
  await page.getByRole("menuitemcheckbox", { name: "Exact STEP export (exact)" }).click();
  await page.getByTestId("export-menu").click();
  await expect(page.getByRole("menuitemcheckbox", { name: "Exact STEP export (exact)" })).toHaveAttribute("aria-checked", "true");
  const [step] = await Promise.all([
    page.waitForEvent("download"),
    page.getByRole("menuitem", { name: "STEP (exact surfaces)…" }).click(),
  ]);
  expect(step.suggestedFilename()).toBe("CSG.step");
  const text = readFileSync(await step.path(), "utf8");
  expect(text.startsWith("ISO-10303-21;")).toBe(true);
  expect(text.trimEnd().endsWith("END-ISO-10303-21;")).toBe(true);
  await expect(summary(page)).toContainText(/Exported CSG\.step \(\d+ bytes\)\. STEP: \d+ of \d+ faces exact/);
  // The toggle is a setting: it survives a reload.
  await page.reload();
  await expect(summary(page)).toContainText("Previewed");
  await page.getByTestId("export-menu").click();
  await expect(page.getByRole("menuitem", { name: "STEP (exact surfaces)…" })).toHaveCount(1);
});

test("the fillet extension is a View menu setting that runs the model again", async ({ page }) => {
  await open(page);
  await expect(summary(page)).toContainText("Previewed");
  await page.getByTestId("view-menu").click();
  const item = page.getByRole("menuitemcheckbox", { name: "Edge fillets and chamfers (fillet)" });
  await expect(item).toHaveAttribute("aria-checked", "false");
  await item.click();
  // Turned on, the document runs again with it.
  await expect(summary(page)).toContainText("Previewed");
  await page.getByTestId("view-menu").click();
  await expect(item).toHaveAttribute("aria-checked", "true");
  await page.keyboard.press("Escape");
  // A setting: it survives a reload.
  await page.reload();
  await expect(summary(page)).toContainText("Previewed");
  await page.getByTestId("view-menu").click();
  await expect(page.getByRole("menuitemcheckbox", { name: "Edge fillets and chamfers (fillet)" })).toHaveAttribute("aria-checked", "true");
});

test("F5 previews instead of reloading, F6 and Mod-Enter render", async ({ page }) => {
  await open(page);
  await page.evaluate(() => (window.__notReloaded = true));
  await page.locator(".cm-content").click();
  await page.keyboard.press("F6");
  await expect(summary(page)).toContainText("Rendered");
  await page.keyboard.press("F5");
  await expect(summary(page)).toContainText("Previewed");
  const before = await editorText(page);
  await page.keyboard.press("ControlOrMeta+Enter");
  await expect(summary(page)).toContainText("Rendered");
  expect(await editorText(page)).toBe(before);
  expect(await page.evaluate(() => window.__notReloaded)).toBe(true);
});

test("console lines jump to their source", async ({ page }) => {
  await open(page);
  await expect(summary(page)).toContainText("Previewed");
  // An echo has no source position (OpenSCAD prints none); a warning does.
  await page.locator(".cm-content").click();
  await page.keyboard.press("ControlOrMeta+End");
  await settleCursor(page, "end");
  await page.keyboard.type("\ncube(size = nosuch);");
  const line = page.locator(".console-line.kind-warning").first();
  await expect(line).toContainText("nosuch");
  await line.click();
  const [from, to] = await page.evaluate(() => window.NeoSCADEditor.state().selection);
  const text = await editorText(page);
  expect(from).toBeLessThan(to);
  expect(text.slice(from, to)).toContain("nosuch");
  // Filters by kind.
  await expect(page.locator(".console-line.kind-echo")).toHaveCount(1);
  await page.getByRole("button", { name: /^Echo \d+/ }).click();
  await expect(page.locator(".console-line.kind-echo")).toHaveCount(0);
});

test("check lists findings and selecting one marks it", async ({ page }) => {
  await open(page, "#example=box-lid");
  await page.getByRole("tab", { name: "Check" }).click();
  const panel = page.getByTestId("check");
  // The box's 2 mm walls pass the default 0.8 mm; a 3 mm minimum fails them.
  await panel.getByLabel("Min wall").fill("3");
  await panel.getByLabel("Min wall").press("Tab");
  await panel.getByRole("button", { name: "Check", exact: true }).click();
  await expect(panel.getByTestId("check-summary")).toBeVisible();
  const findings = panel.locator(".finding");
  await expect(findings.first()).toBeVisible();
  console.log(`box-lid check: ${await findings.count()} findings; first: ${await findings.first().innerText()}`);
  await findings.first().click();
  await expect(findings.first()).toHaveAttribute("aria-pressed", "true");
  await shot(page, "desktop-check");
});

test("measure shows the model, its parts, a section and a distance", async ({ page }) => {
  await open(page, "#example=box-lid");
  await page.getByRole("tab", { name: "Measure" }).click();
  const panel = page.getByTestId("measure");
  await expect(panel.getByRole("checkbox", { name: "Parts" })).toBeChecked();
  await panel.getByRole("button", { name: "Measure", exact: true }).click();
  await expect(panel.getByRole("heading", { name: "Parts", exact: true })).toBeVisible();
  await panel.getByRole("checkbox", { name: "Section" }).check();
  await expect(panel.getByTestId("section-result")).toContainText("area");
  await panel.getByLabel("From").selectOption("box");
  await panel.getByLabel("To").selectOption("lid");
  await expect(panel.getByTestId("between-result")).toContainText("mm");
  await shot(page, "desktop-measure");
});

test("a crashed engine restarts and the page carries on", async ({ page }) => {
  // The wasm core has no way to crash on purpose, so its glue is given
  // one: after an `e2eArmTrap` request, the next run traps (throws the
  // RuntimeError a panic or an OOM throws). The worker's own catch, its
  // `crashed` reply and message, and the page's respawn and replay are the
  // real ones. The mock traps on a `// mock:crash` line instead.
  await page.route("**/core/neoscad_web.js", async (route) => {
    const res = await route.fetch();
    const body =
      (await res.text()) +
      `
let __trap = false;
const __handle = Engine.prototype.handle;
Engine.prototype.handle = function (json, buffers) {
  if (json.includes('"type":"e2eArmTrap"')) {
    __trap = true;
    return [JSON.stringify({ id: JSON.parse(json).id, ok: true, result: {} })];
  }
  if (__trap && json.includes('"type":"run"')) throw new WebAssembly.RuntimeError("unreachable (e2e:trap)");
  return __handle.call(this, json, buffers);
};
`;
    await route.fulfill({ response: res, body });
  });
  await open(page);
  await expect(summary(page)).toContainText("Previewed");
  const mock = (await engineKind(page)) === "mock";
  if (mock) {
    await page.locator(".cm-content").click();
    await page.keyboard.press("ControlOrMeta+End");
    await settleCursor(page, "end");
    await page.keyboard.type("\n// mock:crash");
  } else {
    await page.evaluate(() => window.NeoSCADWeb.engine.request({ type: "e2eArmTrap" }));
    await page.getByTestId("preview").click();
  }
  await expect(page.getByTestId("engine-status")).toHaveText("engine restarted");
  await expect(page.getByTestId("engine-status")).toHaveAttribute("title", /crashed: .*(unreachable|out of memory|mock:crash)/);
  expect(await page.evaluate(() => window.NeoSCADWeb.engine.restarts)).toBe(1);
  if (mock) for (let i = 0; i < "// mock:crash".length; i++) await page.keyboard.press("Backspace");
  else await page.getByTestId("preview").click();
  // The new worker, given the document again, runs the next preview.
  await expect(summary(page)).toContainText("Previewed");
  await expectDrawn(page);
});

test("dark mode uses the dark tokens", async ({ browser }) => {
  const context = await browser.newContext({ colorScheme: "dark", viewport: { width: 1400, height: 860 } });
  const page = await context.newPage();
  await open(page, "#example=sign");
  const bg = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  expect(bg).toBe("rgb(10, 11, 20)");
  await expect(summary(page)).toContainText("Previewed");
  await shot(page, "desktop-dark-sign");
  await context.close();
});

test("a BOSL2 example fetches the library once, then runs", async ({ page }) => {
  const fetched = [];
  page.on("response", (r) => r.url().endsWith("/bosl2.tar.gz") && fetched.push(r.status()));
  await page.goto("/try/#example=gear");
  await page.waitForSelector("html[data-ready]");
  await expect.poll(() => fetched.length).toBeGreaterThan(0);
  test.skip(fetched[0] === 404, "this build has no bosl2.tar.gz (npm run build; use scripts/web/build.sh)");
  await expect(summary(page)).toContainText("Previewed");
  await page.getByTestId("preview").click();
  await expect(summary(page)).toContainText("Previewed");
  expect(fetched).toEqual([200]);
  await shot(page, "desktop-gear");
});

test("#agent opens the Connect your AI agent dialog", async ({ page }) => {
  await open(page, "#agent");
  await expect(page.getByTestId("agent-dialog")).toBeVisible();
  // Taken out of the address bar, as a connect link is.
  expect(new URL(page.url()).hash).toBe("");
});
