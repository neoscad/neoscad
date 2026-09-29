// The page in a browser, served under /try/ with whichever engine the
// build has (the mock until the wasm core lands). Checks the layout, the
// site contract, examples, the editor loop, the console, the customizer
// and its persistence, downloads, F5, the check and measure panels, and a
// respawn. Set E2E_SHOTS=DIR to save screenshots there.

import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";

const shots = process.env.E2E_SHOTS;
const shot = async (page, name) => {
  if (shots) await page.screenshot({ path: `${shots}/${name}.png` });
};

async function open(page, hash = "") {
  const failed = [];
  page.on("response", (r) => {
    if (r.status() >= 400) failed.push(`${r.status()} ${r.url()}`);
  });
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto(`/try/${hash}`);
  await page.waitForSelector("html[data-ready]");
  return { failed, errors };
}

const summary = (page) => page.getByTestId("render-summary");
const editorText = (page) => page.evaluate(() => window.NeoSCADEditor.text().text);

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
  if (!process.env.E2E_SITE) {
    await expect(page.locator("#site-nav a")).toHaveText(["Home", "Try it"]);
    const accent = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--ns-accent").trim());
    expect(accent).toBe("rgb(1, 2, 3)");
  }
  // A drawn view, or the message saying why there is none.
  const drawn = await page.evaluate(() => {
    const c = document.getElementById("viewport");
    const d = c.getContext("2d")?.getImageData(0, 0, c.width, c.height).data;
    if (!d) return null;
    const seen = new Set();
    for (let i = 0; i < d.length; i += 4 * 97) seen.add(`${d[i]},${d[i + 1]},${d[i + 2]}`);
    return seen.size;
  });
  if (drawn === null) await expect(page.getByTestId("view-notice")).toBeVisible();
  else expect(drawn).toBeGreaterThan(3);
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
  const text = readFileSync(await download.path(), "utf8");
  expect(text.startsWith("solid") || text.length > 84).toBeTruthy();
  await page.getByTestId("export-menu").click();
  const [threemf] = await Promise.all([page.waitForEvent("download"), page.getByRole("menuitem", { name: "3MF…" }).click()]);
  expect(threemf.suggestedFilename()).toBe("CSG.3mf");
  expect([...readFileSync(await threemf.path()).subarray(0, 2)]).toEqual([0x50, 0x4b]);
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
  const line = page.locator(".console-line.kind-echo").first();
  await expect(line).toContainText("ECHO:");
  await line.click();
  const [from, to] = await page.evaluate(() => window.NeoSCADEditor.state().selection);
  const text = await editorText(page);
  expect(text.slice(from, to)).toMatch(/^echo\(/);
  // Filters by kind.
  await page.getByRole("button", { name: /^Echo \d+/ }).click();
  await expect(page.locator(".console-line.kind-echo")).toHaveCount(0);
});

test("check lists findings and selecting one marks it", async ({ page }) => {
  await open(page, "#example=box-lid");
  await page.getByRole("tab", { name: "Check" }).click();
  const panel = page.getByTestId("check");
  await panel.getByRole("button", { name: "Check", exact: true }).click();
  await expect(panel.getByTestId("check-summary")).toBeVisible();
  const findings = panel.locator(".finding");
  const n = await findings.count();
  if (n) {
    await findings.first().click();
    await expect(findings.first()).toHaveAttribute("aria-pressed", "true");
  }
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
  await open(page);
  test.skip((await page.evaluate(() => window.NeoSCADWeb.build.engine)) !== "mock", "needs the mock's crash marker");
  await page.locator(".cm-content").click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type("\n// mock:crash");
  await expect(page.getByTestId("engine-status")).toHaveText("engine restarted");
  // Take the marker out again: the new worker runs the next preview.
  for (let i = 0; i < "// mock:crash".length; i++) await page.keyboard.press("Backspace");
  await expect(summary(page)).toContainText("Previewed");
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
