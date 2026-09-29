// What the e2e specs share: opening the page, the render summary, and
// reading the 3D view's pixels.

import { expect } from "@playwright/test";

export const shots = process.env.E2E_SHOTS;

export const shot = async (page, name) => {
  if (shots) await page.screenshot({ path: `${shots}/${name}.png` });
};

export async function open(page, hash = "") {
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

export const summary = (page) => page.getByTestId("render-summary");
export const editorText = (page) => page.evaluate(() => window.NeoSCADEditor.text().text);
export const engineKind = (page) => page.evaluate(() => window.NeoSCADWeb.build.engine);

/// The 3D view's pixels as the screen shows them, whatever draws them
/// (a WebGPU or WebGL canvas cannot be read back with a 2D context): an
/// element screenshot, decoded in the page. Resolves to the number of
/// distinct colours in a sample, and the commonest one (`top`, the
/// background in a drawn view, as "r,g,b" in steps of 8) and its share.
export async function viewPixels(page) {
  const png = await page.locator("#viewport").screenshot();
  return page.evaluate(async (bytes) => {
    const bmp = await createImageBitmap(new Blob([new Uint8Array(bytes)], { type: "image/png" }));
    const c = new OffscreenCanvas(bmp.width, bmp.height);
    const ctx = c.getContext("2d");
    ctx.drawImage(bmp, 0, 0);
    const d = ctx.getImageData(0, 0, bmp.width, bmp.height).data;
    const seen = new Map();
    let n = 0;
    for (let i = 0; i < d.length; i += 4 * 13) {
      const k = `${d[i] >> 3},${d[i + 1] >> 3},${d[i + 2] >> 3}`;
      seen.set(k, (seen.get(k) ?? 0) + 1);
      n += 1;
    }
    const [top, count] = [...seen].reduce((a, b) => (b[1] > a[1] ? b : a));
    return { distinct: seen.size, background: count / n, top };
  }, [...png]);
}

/// A drawn model: many colours (shading), and the background well short
/// of the whole view.
export async function expectDrawn(page) {
  await expect
    .poll(async () => {
      const p = await viewPixels(page);
      return p.distinct > 12 && p.background < 0.97;
    }, { timeout: 10000 })
    .toBe(true);
}

/// Triangles in an STL, ASCII or binary; throws on anything else.
export function stlTriangles(bytes) {
  const head = bytes.subarray(0, 5).toString("latin1");
  const text = bytes.toString("latin1");
  if (head === "solid" && /endsolid/.test(text.slice(-256))) {
    const n = (text.match(/^\s*facet normal /gm) ?? []).length;
    const loops = (text.match(/^\s*endloop/gm) ?? []).length;
    if (n !== loops) throw new Error(`ASCII STL with ${n} facets but ${loops} loops`);
    return n;
  }
  const n = bytes.readUInt32LE(80);
  if (bytes.length !== 84 + 50 * n) throw new Error(`binary STL of ${bytes.length} bytes claims ${n} triangles`);
  return n;
}
