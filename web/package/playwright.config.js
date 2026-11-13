// Playwright for the npm package (web/package): test pages that import the
// built package, self-hosted and from a second origin standing in for a
// CDN (e2e/serve.mjs). It uses web/'s Playwright install (node resolves
// @playwright/test from web/node_modules), so `npm ci` in web/ first.
//
//   node build.mjs --mock --out build-mock && npx playwright test
//   E2E_PACKAGE_REAL=1 E2E_PKG_DIR=build npx playwright test   (the real core
//       and viewer: scripts/web/build-package.sh's staging directory)
//
// Like web/'s, it needs Node 20 or newer. Firefox and WebKit run where
// they are installed (`npx playwright install firefox webkit`) and are
// skipped otherwise.

import { defineConfig, devices } from "@playwright/test";
import { existsSync } from "node:fs";
import { chromium, firefox, webkit } from "@playwright/test";

const port = Number(process.env.E2E_PORT ?? 8124);
const cdnPort = Number(process.env.E2E_CDN_PORT ?? 8125);
const dir = process.env.E2E_PKG_DIR ?? "build-mock";

const installed = (browser) => {
  try {
    return existsSync(browser.executablePath());
  } catch {
    return false;
  }
};

const projects = [
  {
    name: "chromium",
    // The full Chromium (as web/'s config): its new headless mode has a
    // real WebGPU adapter, which the headless shell lacks.
    use: { ...devices["Desktop Chrome"], channel: "chromium" },
  },
];
if (installed(firefox)) projects.push({ name: "firefox", use: { ...devices["Desktop Firefox"] } });
if (installed(webkit)) projects.push({ name: "webkit", use: { ...devices["Desktop Safari"] } });
if (!installed(chromium)) console.warn("package e2e: Chromium is not installed (npx playwright install chromium)");

export default defineConfig({
  testDir: "e2e",
  timeout: process.env.E2E_PACKAGE_REAL ? 90000 : 30000,
  workers: 1,
  reporter: [["list"]],
  outputDir: process.env.E2E_OUT ?? "test-results",
  use: { baseURL: `http://127.0.0.1:${port}` },
  metadata: { cdnPort },
  webServer: {
    command: `node e2e/serve.mjs --dir ${JSON.stringify(dir)} --port ${port} --cdn-port ${cdnPort}`,
    url: `http://127.0.0.1:${port}/index.html`,
    reuseExistingServer: false,
  },
  projects,
});
