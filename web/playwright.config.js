// Playwright against a built bundle served under /try/ (serve.mjs), as the
// website serves it. Build first (`npm run build`, or scripts/web/build.sh
// and E2E_DIR=../dist/web/neoscad-web-...). Chromium for the app's specs;
// the agent bridge's spec also runs in Firefox and WebKit (the projects
// below; `npx playwright install firefox webkit`).
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
    // The agent bridge must work in every desktop engine, so its spec runs
    // in Firefox and WebKit too (`npx playwright install firefox webkit`;
    // it needs NEOSCAD_BIN, and skips without it). The other specs are
    // written against Chromium's WebGPU viewer.
    {
      name: "firefox",
      use: { ...devices["Desktop Firefox"], viewport: { width: 1400, height: 860 } },
      testMatch: /agent\.spec/,
    },
    {
      name: "webkit",
      use: { ...devices["Desktop Safari"], viewport: { width: 1400, height: 860 } },
      testMatch: /agent\.spec/,
    },
  ],
});
