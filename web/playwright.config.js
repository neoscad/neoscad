// Playwright against a built bundle served under /try/ (serve.mjs), as the
// website serves it. Build first (`npm run build`, or scripts/web/build.sh
// and E2E_DIR=../dist/web/neoscad-web-...). Every desktop spec runs in
// Chromium, Firefox and WebKit (`npx playwright install chromium firefox
// webkit`); CI runs them against the mock build (.github/workflows/ci.yml,
// `web`).
//
// Needs Node 20 or newer (Playwright 1.63's requirement), unlike the unit
// tests, which run on 18.

import { defineConfig, devices } from "@playwright/test";

const port = Number(process.env.E2E_PORT ?? 8123);
const dir = process.env.E2E_DIR ?? "dist";
// E2E_URL: a server already running (the website under `python3 -m
// http.server`, say) whose /try/ is the bundle; serve.mjs is not started.
const external = process.env.E2E_URL ?? null;

export default defineConfig({
  testDir: "e2e",
  timeout: 30000,
  workers: 1,
  reporter: [["list"]],
  outputDir: process.env.E2E_OUT ?? "test-results",
  use: {
    baseURL: external ?? `http://127.0.0.1:${port}`,
    acceptDownloads: true,
  },
  webServer: external ? undefined : {
    // E2E_SITE: a copy of the website to serve at the root (its site.json
    // and theme.css) instead of serve.mjs's stand-ins.
    command: `node serve.mjs --dir ${JSON.stringify(dir)} --port ${port}${process.env.E2E_SITE ? ` --site ${JSON.stringify(process.env.E2E_SITE)}` : ""}`,
    url: `http://127.0.0.1:${port}/try/`,
    reuseExistingServer: false,
  },
  // `channel: "chromium"` runs the full Chromium in the new headless mode:
  // it gives WebGPU a real (Metal) adapter, where the default headless
  // shell has `navigator.gpu` but no adapter, so the WebGPU viewer would
  // never be the one tested.
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"], channel: "chromium", viewport: { width: 1400, height: 860 } },
      testIgnore: /phone\.spec/,
    },
    { name: "phone", use: { ...devices["Pixel 7"], channel: "chromium" }, testMatch: /phone\.spec/ },
    // The page must work in every desktop engine, so every desktop spec
    // runs in Firefox and WebKit too (the agent's needs NEOSCAD_BIN, and
    // skips without it). Their viewer is whichever the browser starts:
    // Playwright's WebKit on macOS has a WebGPU adapter, and its Firefox
    // has `navigator.gpu` but "WebGPU is disabled by blocklist" (no
    // adapter, even with the blocklist prefs), so Firefox tests the page's
    // fallback to the WebGL build, and skips the one WebGPU-only test.
    {
      name: "firefox",
      use: { ...devices["Desktop Firefox"], viewport: { width: 1400, height: 860 } },
      testIgnore: /phone\.spec/,
    },
    {
      name: "webkit",
      use: { ...devices["Desktop Safari"], viewport: { width: 1400, height: 860 } },
      testIgnore: /phone\.spec/,
    },
  ],
});
