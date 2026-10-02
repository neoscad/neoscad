// "Connect your AI agent" end to end: a real `neoscad mcp --browser`
// (NEOSCAD_BIN, built from this checkout) driven over MCP stdio as an agent
// drives it, and the page connected to it, directly and through the relay
// window. Runs in Chromium, Firefox and WebKit (playwright.config.js).
//
// The page is served from 127.0.0.1 here, where every browser allows the
// direct socket; what an https page on neoscad.org meets (WebKit's mixed
// content block, Chrome's local network prompt) is in
// docs/agent-bridge.md, with how it was checked.

import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { expect, test } from "@playwright/test";
import { editorText, open, shot } from "./helpers.js";

const BIN = process.env.NEOSCAD_BIN;
test.skip(!BIN, "set NEOSCAD_BIN to a neoscad built with the bridge");

/// `neoscad mcp --browser` for the page this suite serves, and a minimal
/// MCP client for it.
class Agent {
  constructor(baseURL) {
    this.proc = spawn(BIN, ["mcp", "--browser", "--browser-url", `${baseURL}/try/`], { stdio: ["pipe", "pipe", process.env.E2E_BRIDGE_LOG ? "inherit" : "ignore"] });
    this.next = 1;
    this.waiting = new Map();
    createInterface({ input: this.proc.stdout }).on("line", (line) => {
      const m = JSON.parse(line);
      this.waiting.get(m.id)?.(m);
      this.waiting.delete(m.id);
    });
  }

  call(method, params = {}) {
    const id = this.next++;
    this.proc.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    return new Promise((resolve) => this.waiting.set(id, resolve));
  }

  async tool(name, args = {}) {
    const r = await this.call("tools/call", { name, arguments: args });
    return r.result;
  }

  async text(name, args) {
    return (await this.tool(name, args)).content[0].text;
  }

  /// The page path of the connect link (the server's link names this
  /// suite's origin).
  async link() {
    const t = await this.text("browser_connect");
    return t.slice(t.indexOf("/try/#connect=")).split(/\s/)[0];
  }

  stop() {
    this.proc.kill();
  }
}

let agent;
test.beforeEach(async ({ baseURL, page }) => {
  if (process.env.E2E_BRIDGE_LOG) page.on("console", (m) => console.log(`page: ${m.text()}`));
  agent = new Agent(baseURL);
  await agent.call("initialize", { protocolVersion: "2025-11-25", capabilities: {}, clientInfo: { name: "e2e-agent" } });
});
test.afterEach(() => agent.stop());

const connected = (page) => expect(page.locator("html")).toHaveAttribute("data-agent", "connected", { timeout: 15000 });

test("an agent reads, edits, sees and points at the page", async ({ page, browserName }) => {
  const link = await agent.link();
  // The agent waits for the page in one call instead of polling.
  const waited = agent.text("browser_connect", { wait_seconds: 25 });
  const { errors } = await open(page, link.slice("/try/".length));
  expect(await waited).toMatch(/^Connected: the web page's \S+\.scad/);
  await connected(page);
  // The key leaves the address bar.
  expect(page.url()).not.toContain("connect=");
  await expect(page.getByTestId("agent-button")).toContainText("e2e-agent connected");
  expect(await agent.text("browser_connect")).toMatch(/^Connected: the web page's \S+\.scad in \S+/);

  // editor_read gives the editor's text, numbered, and its version.
  const text = await editorText(page);
  const read = await agent.text("editor_read");
  const version = Number(read.match(/, version (\d+),/)[1]);
  const firstLine = text.split("\n")[0];
  expect(read).toContain(`     1\t${firstLine}\n`);

  // An edit lands in the editor, highlighted, as one undoable step.
  const r = await agent.tool("editor_edit", { version, edits: [{ at: [1, 1, 1, 1], new: "// edited by the agent\n" }] });
  expect(r.isError, JSON.stringify(r)).toBe(false);
  await expect.poll(() => editorText(page)).toBe(`// edited by the agent\n${text}`);
  expect(await page.evaluate(() => window.NeoSCADEditor.agentMarks())).toBeGreaterThan(0);
  await shot(page, `agent-edit-${browserName}`);
  // A stale version is refused.
  const stale = await agent.tool("editor_edit", { version, edits: [{ at: [1, 1, 1, 1], new: "x" }] });
  expect(stale.isError).toBe(true);
  expect(stale.content[0].text).toContain("changed since version");
  // The model tools default to the page's text.
  expect(await agent.text("evaluate")).toMatch(/^the web page's \S+ \(version \d+\)/);

  // The camera, marks and a capture of what the user sees.
  const cam = await agent.tool("view_camera", { vpd: 321 });
  expect(cam.content[0].text).toContain("$vpd = 321");
  expect(await page.evaluate(() => window.NeoSCADWeb.viewer.camera().vpd)).toBeCloseTo(321, 3);
  await agent.tool("view_camera", { fit: true });
  const marks = await agent.text("view_annotate", { markers: [{ point: [0, 0, 0], label: "origin" }] });
  expect(marks).toContain("showing 1 marker");
  const cap = await agent.tool("view_capture", { size: 400 });
  expect(cap.isError, JSON.stringify(cap).slice(0, 400)).toBe(false);
  const png = Buffer.from(cap.content[1].data, "base64");
  expect(png.subarray(1, 4).toString()).toBe("PNG");
  const [w, h] = [png.readUInt32BE(16), png.readUInt32BE(20)];
  expect(Math.max(w, h)).toBeLessThanOrEqual(400);
  expect(Math.max(w, h)).toBeGreaterThan(100);
  expect(await agent.text("console_read")).toBeTruthy();
  const reveal = await agent.text("editor_reveal", { at: [2] });
  expect(reveal).toContain("showing 2:1");

  // Undo in the editor takes the agent's edit back in one step.
  await page.evaluate(() => window.NeoSCADEditor.undo());
  await expect.poll(() => editorText(page)).toBe(text);

  // Disconnect in the page: the tools say so.
  await page.getByTestId("agent-button").click();
  await page.getByTestId("agent-disconnect").click();
  await expect(page.locator("html")).toHaveAttribute("data-agent", "idle");
  expect(await agent.text("editor_read")).toContain("browser_connect");
  expect(errors).toEqual([]);
});

test("through the connection window, with edits the user approves", async ({ page, browserName }) => {
  const link = await agent.link();
  await open(page, `${link.slice("/try/".length)}&via=relay`);
  // The dialog offers the window; a click opens it.
  await expect(page.getByTestId("agent-dialog")).toBeVisible();
  await shot(page, `agent-offer-relay-${browserName}`);
  const popupOpened = page.waitForEvent("popup");
  await page.getByTestId("agent-open").click();
  const popup = await popupOpened;
  await connected(page);
  await expect(popup.locator("#state")).toContainText("Connected");
  expect(await agent.text("browser_connect")).toContain("through its connection window");

  // Ask before applying: the page asks, and Reject leaves the text alone.
  await page.getByTestId("agent-ask").check();
  const text = await editorText(page);
  const version = Number((await agent.text("editor_read")).match(/, version (\d+),/)[1]);
  const rejected = agent.tool("editor_edit", { version, edits: [{ at: [1, 1, 1, 1], new: "// no\n" }] });
  await page.getByTestId("agent-reject").click();
  const r = await rejected;
  expect(r.isError).toBe(true);
  expect(r.content[0].text).toContain("declined");
  expect(await editorText(page)).toBe(text);
  const applied = agent.tool("editor_edit", { version, edits: [{ at: [1, 1, 1, 1], new: "// yes\n" }] });
  await expect(page.getByTestId("agent-confirm")).toBeVisible();
  await shot(page, `agent-confirm-${browserName}`);
  await page.getByTestId("agent-apply").click();
  expect((await applied).isError).toBe(false);
  await expect.poll(() => editorText(page)).toBe(`// yes\n${text}`);

  // Closing the window disconnects.
  await popup.close();
  await expect(page.locator("html")).toHaveAttribute("data-agent", "closed", { timeout: 5000 });
});

test("a second tab takes the connection over", async ({ page, context }) => {
  const link = await agent.link();
  await open(page, link.slice("/try/".length));
  await connected(page);
  const second = await context.newPage();
  await open(second, link.slice("/try/".length));
  await connected(second);
  await expect(page.locator("html")).toHaveAttribute("data-agent", "closed");
  await page.getByTestId("agent-button").click();
  await expect(page.getByTestId("agent-status")).toContainText("Another tab took over");
});

test("the dialog explains, in light and dark", async ({ page, browserName }) => {
  await open(page);
  const button = page.getByTestId("agent-button");
  await expect(button).toHaveText(/Connect your AI agent/);
  await button.focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByTestId("agent-dialog");
  await expect(dialog).toBeVisible();
  await expect(page.getByTestId("agent-setup-code")).toHaveText("claude mcp add neoscad -- neoscad mcp --browser");
  await page.getByRole("tab", { name: "VS Code" }).click();
  await expect(page.getByTestId("agent-setup-code")).toContainText('"servers"');
  await shot(page, `agent-dialog-light-${browserName}`);
  await page.emulateMedia({ colorScheme: "dark" });
  await shot(page, `agent-dialog-dark-${browserName}`);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await shot(page, `agent-button-dark-${browserName}`);
});

// What Chrome does for neoscad.org once the user has said no to "Apps on
// device": the page is made a public site (as the real one is), so the
// socket to 127.0.0.1 goes through Local Network Access, and headless
// Chromium answers its prompt with "deny". The first link fails and
// offers the window with the hint; after that the browser reports the
// permission denied, and the next link skips the doomed attempt.
test("a refused local network permission goes straight to the window", async ({ playwright, baseURL, browserName }) => {
  test.skip(browserName !== "chromium", "Local Network Access is Chromium's");
  const port = new URL(baseURL).port;
  const browser = await playwright.chromium.launch({
    channel: "chromium",
    args: [`--ip-address-space-overrides=127.0.0.1:${port}=public`],
  });
  try {
    // One context: the refusal is remembered per site, across its tabs.
    const context = await browser.newContext({ viewport: { width: 1400, height: 860 } });
    const blocked = [];
    const newPage = async () => {
      const p = await context.newPage();
      p.on("console", (m) => /LOCAL_NETWORK_ACCESS/.test(m.text()) && blocked.push(m.text()));
      return p;
    };
    let page = await newPage();
    const link = (await agent.link()).slice("/try/".length);
    await open(page, link);
    await expect(page.locator("html")).toHaveAttribute("data-agent", "failed", { timeout: 15000 });
    expect(blocked.length).toBeGreaterThan(0);
    await expect(page.getByTestId("agent-allow-hint")).toContainText("Site settings");
    expect(await page.evaluate(() => navigator.permissions.query({ name: "loopback-network" }).then((s) => s.state))).toBe("denied");

    // The next link (a new tab: the same URL again would only change the
    // fragment): no direct attempt, the window at once.
    blocked.length = 0;
    page = await newPage();
    await open(page, link);
    await expect(page.locator("html")).toHaveAttribute("data-agent", "failed");
    await expect(page.getByTestId("agent-open")).toBeVisible();
    expect(blocked).toEqual([]);
    const waited = agent.text("browser_connect", { wait_seconds: 25 });
    const popupOpened = page.waitForEvent("popup");
    await page.getByTestId("agent-open").click();
    const popup = await popupOpened;
    await expect(popup).toHaveTitle(/keep this window open/);
    await connected(page);
    const text = await waited;
    expect(text).toContain("through its connection window");
    expect(text).toContain("keep that small window open");
  } finally {
    await browser.close();
  }
});
