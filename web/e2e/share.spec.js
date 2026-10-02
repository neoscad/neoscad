// Models in the address (src/share.js) and the embed view (src/embed.js),
// in Chromium, Firefox and WebKit (playwright.config.js): a `#code=` link
// opens as a document of its own and leaves the address bar and the
// visitor's storage alone; Copy link makes such a link; `#embed=1` is the
// 3D view alone, framed by a page from this origin and refused by another;
// and the top bar fits one row from 1200 px.

import { createServer } from "node:http";
import { deflateRawSync } from "node:zlib";
import { expect, test } from "@playwright/test";
import { editorText, expectDrawn, open, shot, summary } from "./helpers.js";

const SOURCE = `// Shared from a link: ünïcödé 🧊
difference() {
  cube(20, center = true);
  sphere(r = 13);
}
`;

const b64url = (buf) => Buffer.from(buf).toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const plain = (text) => b64url(Buffer.from(text, "utf8"));
const packed = (text) => `z:${b64url(deflateRawSync(Buffer.from(text, "utf8")))}`;

/// The page's own entries in localStorage, as a sorted object.
const stored = (page) =>
  page.evaluate(() =>
    Object.fromEntries(
      Object.keys(localStorage)
        .filter((k) => k.startsWith("neoscad.try."))
        .sort()
        .map((k) => [k, localStorage.getItem(k)]),
    ),
  );

/// `open` as a new document: a goto that changes only the fragment would
/// not load the page again.
async function fresh(page, hash = "") {
  await page.goto("about:blank");
  return open(page, hash);
}

/// The embed view loaded and run (or failed: data-ran says which).
async function openEmbed(page, hash) {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("about:blank");
  await page.goto(`/try/${hash}`);
  await page.waitForSelector("html[data-ran]");
  return errors;
}

test("a #code= link opens as its own document, and saves nothing", async ({ page }) => {
  // A visitor with an edited example and settings of their own.
  await fresh(page, "#example=menger");
  await page.evaluate(() => {
    localStorage.setItem("neoscad.try.v1.example.csg.text", JSON.stringify("// my own CSG edits\ncube(3);"));
  });
  const before = await stored(page);
  expect(Object.keys(before).length).toBeGreaterThan(1);

  for (const [form, payload] of [["compressed", packed(SOURCE)], ["plain", plain(SOURCE)]]) {
    const { errors } = await fresh(page, `#code=${payload}&name=${encodeURIComponent("my part")}`);
    expect(await editorText(page), form).toBe(SOURCE);
    await expect(page.locator("#editor-tabs [role=tab]").first()).toHaveText("my part.scad");
    await expect(page.getByTestId("example-picker")).toHaveValue("#link");
    await expect(summary(page)).toContainText("Previewed");
    // Out of the address bar at once.
    expect(await page.evaluate(() => location.hash)).toBe("");
    // Edit it, and change its customizer-free settings: nothing it does
    // reaches storage but the view settings the page always keeps.
    await page.locator(".cm-content").click();
    await page.keyboard.press("ControlOrMeta+End");
    await page.keyboard.type("\ncube(1);");
    await expect.poll(() => editorText(page)).toContain("cube(1);");
    await page.waitForTimeout(500);
    expect(await stored(page)).toEqual(before);
    expect(errors).toEqual([]);
  }

  // The visitor's example and its edits are where they were.
  await page.getByTestId("example-picker").selectOption("csg");
  await expect.poll(() => editorText(page)).toBe("// my own CSG edits\ncube(3);");
  // The link's document stays in the picker, with its edits, until the
  // page is closed.
  await page.getByTestId("example-picker").selectOption("#link");
  await expect.poll(() => editorText(page)).toContain("cube(1);");
  // A reload opens the visitor's last example, not the link.
  await page.reload();
  await page.waitForSelector("html[data-ready]");
  // (The visitor chose CSG in the picker, which is what a reload reopens.)
  await expect(page.getByTestId("example-picker")).toHaveValue("csg");
  await expect(page.getByTestId("example-picker").locator("option[value='#link']")).toHaveCount(0);
});

test("a bad #code= payload says so and opens the usual example", async ({ page }) => {
  const cases = {
    "not base64url": "#code=%25%25%25",
    damaged: `#code=z:${b64url(Buffer.from([0xff, 0xff, 0xff, 0xff]))}`,
    "too long": `#code=${"A".repeat(100000)}`,
    "a zip bomb": `#code=z:${b64url(deflateRawSync(Buffer.alloc(8 * 1024 * 1024)))}`,
  };
  for (const [what, hash] of Object.entries(cases)) {
    const { errors } = await fresh(page, hash);
    await expect(page.locator("#banner"), what).toContainText("The link's model could not be opened");
    await expect(page.getByTestId("example-picker")).not.toHaveValue("#link");
    expect(await page.evaluate(() => location.hash), what).not.toContain("code=");
    expect(errors, what).toEqual([]);
  }
});

test("Copy link makes a link that opens the same text", async ({ page, context, browserName }) => {
  if (browserName === "chromium") await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  // Where the browser will not write the clipboard, the page asks the
  // visitor to copy the link from a prompt instead.
  page.on("dialog", (d) => d.accept());
  await fresh(page, "#example=csg");
  await page.locator(".cm-content").click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type("\n// shared on purpose");
  await expect.poll(() => editorText(page)).toContain("// shared on purpose");
  const text = await editorText(page);
  for (const [item, embed] of [["Copy link", false], ["Copy embed link", true]]) {
    await page.getByTestId("export-menu").click();
    await page.getByRole("menuitem", { name: item, exact: true }).click();
    await expect.poll(() => page.evaluate(() => window.NeoSCADWeb.lastLink)).toContain(embed ? "#embed=1&code=z:" : "#code=z:");
    const link = await page.evaluate(() => window.NeoSCADWeb.lastLink);
    expect(link).toContain("&name=CSG.scad");
    const other = await context.newPage();
    if (embed) {
      await other.goto(link);
      await other.waitForSelector("html[data-ran=ok]");
      expect(await other.evaluate(() => window.NeoSCADEmbed.doc.text)).toBe(text);
    } else {
      await other.goto(link);
      await other.waitForSelector("html[data-ready]");
      expect(await editorText(other)).toBe(text);
    }
    await other.close();
  }
});

test("the embed view: the 3D view alone, previewed, saving nothing", async ({ page }) => {
  // A new context: the storage starts empty, and must stay so.
  const errors = await openEmbed(page, `#embed=1&code=${packed(SOURCE)}&name=part`);
  expect(errors).toEqual([]);
  await expect(page.locator("html")).toHaveAttribute("data-ran", "ok");
  // No top bar, editor, console or inspector; the view fills the frame.
  for (const sel of ["#topbar", "#pane-left", "#inspector", "#mobile-tabs"]) await expect(page.locator(sel)).toBeHidden();
  const view = await page.locator("#viewport").boundingBox();
  const size = page.viewportSize();
  expect(view.width).toBeGreaterThan(size.width - 2);
  expect(view.height).toBeGreaterThan(size.height - 2);
  await expectDrawn(page);
  // Light: no editor (so no language server) and no agent bridge.
  expect(await page.evaluate(() => [typeof window.NeoSCADEditor, typeof window.NeoSCADWeb])).toEqual(["undefined", "undefined"]);
  await expect(page.getByTestId("agent-button")).toHaveCount(0);
  // The whole page's link to the same model, in a new tab.
  const link = page.getByTestId("embed-open");
  await expect(link).toHaveAttribute("target", "_blank");
  await expect(link).toHaveAttribute("href", /\/try\/#code=z:[A-Za-z0-9_-]+&name=part\.scad$/);
  // The fragment stays: the frame must reload to the same model.
  expect(await page.evaluate(() => location.hash)).toContain("embed=1");
  expect(await stored(page)).toEqual({});
  await shot(page, "embed-code");

  // An example, as shipped.
  await page.evaluate(() => localStorage.setItem("neoscad.try.v1.example.csg.text", JSON.stringify("cube(1); // edited")));
  await openEmbed(page, "#embed=1&example=csg");
  await expect(page.locator("html")).toHaveAttribute("data-ran", "ok");
  expect(await page.evaluate(() => window.NeoSCADEmbed.doc.text)).toMatch(/^\/\/ CSG\.scad/);
  await expect(page.getByTestId("embed-open")).toHaveAttribute("href", /\/try\/#example=csg$/);
  expect(Object.keys(await stored(page))).toEqual(["neoscad.try.v1.example.csg.text"]);
});

test("the embed view says what is wrong with a bad link", async ({ page }) => {
  const errors = await openEmbed(page, "#embed=1&code=%25%25");
  expect(errors).toEqual([]);
  await expect(page.locator("html")).toHaveAttribute("data-ran", "failed");
  await expect(page.getByTestId("embed-status")).toContainText("The link's model could not be opened");
  await openEmbed(page, "#embed=1&example=nope");
  await expect(page.getByTestId("embed-status")).toContainText('There is no example "nope"');
});

/// A page on `origin` that frames `src`, as a blog post does (its CSP
/// allows frames from its own origin only).
async function framingPage(page, origin, src) {
  const url = `${origin}/framing-test.html`;
  await page.route(url, (route) =>
    route.fulfill({
      contentType: "text/html",
      body: `<!doctype html><meta http-equiv="Content-Security-Policy" content="default-src 'self'; frame-src 'self'">
<iframe id="f" src="${src}" style="width:640px;height:400px;border:0"></iframe>`,
    }),
  );
  await page.goto(url);
  return page.frameLocator("#f");
}

test("framed by a page from the same origin it works; from another it refuses", async ({ page, baseURL }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  const frame = await framingPage(page, baseURL, `/try/#embed=1&code=${packed(SOURCE)}`);
  await expect(frame.locator("html[data-ran=ok]")).toHaveCount(1);
  await expect(frame.getByTestId("embed-open")).toBeVisible();
  await shot(page, "embed-framed");

  // Another origin: a server of its own on another port. (Not a routed
  // page: Chromium treats one as public, and its local network checks
  // would block the frame before the page could refuse it.) It sends no
  // CSP, as another site's page needn't.
  const other = createServer((req, res) => {
    const src = req.url === "/page.html" ? `${baseURL}/try/` : `${baseURL}/try/#embed=1&example=csg`;
    res.writeHead(200, { "content-type": "text/html" });
    res.end(`<!doctype html><iframe id="f" src="${src}" style="width:640px;height:400px"></iframe>`);
  });
  await new Promise((r) => other.listen(0, "127.0.0.1", r));
  const origin = `http://127.0.0.1:${other.address().port}`;
  try {
    await page.goto(`${origin}/embed.html`);
    const refused = page.frameLocator("#f");
    await expect(refused.getByTestId("refused")).toBeVisible();
    await expect(refused.locator("#viewport")).toHaveCount(0);
    await expect(refused.locator("a")).toHaveAttribute("target", "_blank");
    // The whole page too, not only the embed view.
    await page.goto(`${origin}/page.html`);
    await expect(page.frameLocator("#f").getByTestId("refused")).toBeVisible();
  } finally {
    other.close();
  }
  expect(errors).toEqual([]);
});

/// The top bar's items: whether any two overlap, the spread of their
/// vertical centres (one row: a few pixels), whether the agent button's
/// label is cut off, and whether anything runs past the window.
const topBar = (page) =>
  page.evaluate(() => {
    const items = [...document.querySelectorAll("#topbar > *:not(.tools), #topbar .tools > *")]
      .filter((e) => e.offsetParent && e.getBoundingClientRect().width > 0)
      .map((e) => e.getBoundingClientRect());
    let overlap = false;
    for (let i = 0; i < items.length; i++)
      for (let j = i + 1; j < items.length; j++) {
        const [a, b] = [items[i], items[j]];
        if (a.left < b.right - 1 && b.left < a.right - 1 && a.top < b.bottom - 1 && b.top < a.bottom - 1) overlap = true;
      }
    const centres = items.map((r) => r.top + r.height / 2);
    const agent = document.querySelector(".agent-button");
    return {
      spread: Math.max(...centres) - Math.min(...centres),
      overlap,
      clipped: agent.scrollWidth > agent.clientWidth + 1,
      past: items.some((r) => r.right > innerWidth + 1),
    };
  });

test("the top bar is one row from 1200 px, and nothing in it is cut off", async ({ page }) => {
  for (const width of [1200, 1280, 1440, 1920, 1024, 768]) {
    await page.setViewportSize({ width, height: 800 });
    await fresh(page);
    // Idle, and busy: a run shows Cancel and a spinner, the widest the
    // bar gets.
    for (const state of ["idle", "working"]) {
      if (state === "working") {
        // As engineStatus() draws it, set directly: the engine's own
        // status changes (a parameters request after the preview) would
        // race a real one.
        await expect(summary(page)).toContainText("Previewed");
        await page.evaluate(() => {
          const app = window.NeoSCADWeb;
          app.engineStatus = () => {};
          app.cancelButton.hidden = false;
          app.status.innerHTML = '<span class="spinner" aria-hidden="true"></span>working…';
        });
      }
      const m = await topBar(page);
      const at = `${width} px, ${state}`;
      expect(m.overlap, at).toBe(false);
      expect(m.clipped, at).toBe(false);
      expect(m.past, at).toBe(false);
      if (width >= 1200) expect(m.spread, at).toBeLessThan(6);
      else expect(m.spread, at).toBeGreaterThan(12);
    }
  }
});
