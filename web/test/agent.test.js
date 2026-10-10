// The agent connection (src/agent/): the connect link, and the protocol
// over a direct socket and through the relay window, with stand-ins for
// the WebSocket and the window. The real page against a real
// `neoscad mcp --browser` is e2e/agent.spec.js.

import assert from "node:assert/strict";
import { test } from "node:test";
import { AgentConnection, REPLACED } from "../src/agent/connection.js";
import { PageAgent } from "../src/agent/page.js";
import {
  agentLine,
  allowDirectHint,
  browserName,
  captureSize,
  diagnostics,
  loopbackDenied,
  parseConnect,
  relayURL,
  stripConnect,
  toBase64,
  wsURL,
} from "../src/agent/link.js";

const TOKEN = "0123456789abcdef0123456789abcdef";
const tick = () => new Promise((r) => setImmediate(r));

test("connect links parse from a URL, a fragment or a pasted key", () => {
  const want = { port: 51234, token: TOKEN };
  assert.deepEqual(parseConnect(`https://neoscad.org/try/#connect=51234.${TOKEN}`), want);
  assert.deepEqual(parseConnect(`#example=gears&connect=51234.${TOKEN}`), want);
  assert.deepEqual(parseConnect(`  51234.${TOKEN.toUpperCase()}\n`), want);
  assert.equal(parseConnect(`#connect=51234.${TOKEN.slice(1)}`), null);
  assert.equal(parseConnect(`#connect=70000.${TOKEN}`), null);
  assert.equal(parseConnect(`#connect=0.${TOKEN}`), null);
  assert.equal(parseConnect("#example=gears"), null);
  assert.equal(parseConnect(null), null);
  assert.equal(wsURL(want), `ws://127.0.0.1:51234/ws?token=${TOKEN}`);
  assert.equal(relayURL(want), `http://127.0.0.1:51234/relay#${TOKEN}`);
});

test("the link leaves the fragment, the rest stays", () => {
  assert.equal(stripConnect(`#connect=1.${TOKEN}`), "");
  assert.equal(stripConnect(`#example=gears&connect=1.${TOKEN}`), "#example=gears");
  assert.equal(stripConnect(""), "");
});

test("browser names, capture sizes, console lines and base64", () => {
  assert.equal(browserName("Mozilla/5.0 (Macintosh) Gecko/20100101 Firefox/155.0"), "Firefox 155");
  assert.equal(browserName("Mozilla/5.0 AppleWebKit/537.36 Chrome/154.0.0.0 Safari/537.36"), "Chrome 154");
  assert.equal(browserName("Mozilla/5.0 Chrome/154.0.0.0 Safari/537.36 Edg/154.0.0.0"), "Edge 154");
  assert.equal(browserName("Mozilla/5.0 AppleWebKit/605.1.15 Version/27.0 Safari/605.1.15"), "Safari 27");
  assert.deepEqual(captureSize(1600, 900, 768), [768, 432]);
  assert.deepEqual(captureSize(400, 300, 768), [400, 300]);
  assert.deepEqual(captureSize(0, 0, 768), [1, 1]);
  const lines = [
    { kind: "echo", text: "ECHO: 1", location: null },
    { kind: "warning", text: "w", location: { path: "/doc.scad", startLine: 4 } },
    { kind: "error", text: "e", location: { path: "/lib/x.scad", startLine: 0 } },
  ];
  assert.deepEqual(diagnostics(lines, "/doc.scad"), [
    { kind: "warning", text: "w", line: 5 },
    { kind: "error", text: "e", line: 1, file: "x.scad" },
  ]);
  assert.deepEqual(agentLine(lines[0], "/doc.scad"), { kind: "echo", text: "ECHO: 1" });
  assert.equal(toBase64(new Uint8Array([0x89, 0x50, 0x4e, 0x47])), "iVBORw==");
  assert.equal(toBase64(new Uint8Array(100000)).length, 133336);
});

class FakeSocket {
  static last = null;
  constructor(url) {
    this.url = url;
    this.sent = [];
    FakeSocket.last = this;
  }
  send(t) {
    this.sent.push(JSON.parse(t));
  }
  close() {
    this.closed = true;
  }
  open() {
    this.onopen?.();
  }
  message(m) {
    this.onmessage?.({ data: JSON.stringify(m) });
  }
  end(code, reason = "") {
    this.onclose?.({ code, reason });
  }
}

function direct(handle = async (m, p) => ({ method: m, params: p })) {
  const states = [];
  const c = new AgentConnection({
    handle,
    onChange: (s) => states.push(s),
    hello: () => ({ file: "a.scad" }),
    WebSocketImpl: FakeSocket,
    win: {},
  });
  return { c, states };
}

test("a direct connection says hello and answers requests", async () => {
  const { c, states } = direct(async (m, p) => {
    if (m === "boom") throw new Error("no such thing");
    return { echoed: p.x };
  });
  c.connectDirect({ port: 5, token: TOKEN });
  const ws = FakeSocket.last;
  assert.equal(ws.url, `ws://127.0.0.1:5/ws?token=${TOKEN}`);
  assert.equal(states.at(-1).status, "connecting");
  ws.open();
  assert.equal(states.at(-1).status, "connected");
  assert.deepEqual(ws.sent[0], { type: "hello", file: "a.scad" });
  ws.message({ type: "welcome", client: "Claude Code", server: "neoscad 1" });
  assert.equal(c.state.client, "Claude Code");
  ws.message({ id: 7, method: "read", params: { x: 3 } });
  ws.message({ id: 8, method: "boom", params: {} });
  await tick();
  assert.deepEqual(ws.sent[1], { id: 7, result: { echoed: 3 } });
  assert.deepEqual(ws.sent[2], { id: 8, error: { message: "no such thing" } });
  // Replaced by another tab.
  ws.end(REPLACED, "another tab connected");
  assert.deepEqual([c.state.status, c.state.reason], ["closed", "replaced"]);
});

test("a blocked direct connection fails, and old sockets are ignored", () => {
  const { c } = direct();
  c.connectDirect({ port: 5, token: TOKEN });
  const first = FakeSocket.last;
  first.end(1006);
  assert.deepEqual([c.state.status, c.state.reason], ["failed", "direct"]);
  // WebKit's mixed-content block: `error` and no `close`.
  c.connectDirect({ port: 5, token: TOKEN });
  FakeSocket.last.onerror();
  assert.deepEqual([c.state.status, c.state.reason], ["failed", "direct"]);
  c.connectDirect({ port: 5, token: TOKEN });
  first.onopen?.();
  assert.equal(c.state.status, "connecting");
  c.disconnect();
  assert.equal(FakeSocket.last.closed, true);
  assert.equal(c.state.status, "idle");
});

test("the relay window is trusted only from its own origin", async () => {
  const listeners = [];
  const popup = { closed: false, posted: [], postMessage(m, o) { this.posted.push([m, o]); }, close() { this.closed = true; } };
  const win = {
    open: (url) => ((popup.url = url), popup),
    addEventListener: (_t, f) => listeners.push(f),
    removeEventListener: (_t, f) => listeners.splice(listeners.indexOf(f), 1),
  };
  const c = new AgentConnection({ handle: async () => ({ ok: 1 }), win, hello: () => ({ file: "a.scad" }) });
  const link = { port: 6, token: TOKEN };
  c.connectRelay(link);
  assert.equal(popup.url, `http://127.0.0.1:6/relay#${TOKEN}`);
  const from = (origin, data, source = popup) => listeners.forEach((f) => f({ origin, source, data }));
  from("https://evil.example", { neoscadRelay: 1, event: "open" });
  from("http://127.0.0.1:6", { neoscadRelay: 1, event: "open" }, {});
  assert.equal(c.state.status, "connecting");
  from("http://127.0.0.1:6", { neoscadRelay: 1, event: "open" });
  assert.equal(c.state.status, "connected");
  assert.equal(c.state.via, "relay");
  from("http://127.0.0.1:6", { neoscadRelay: 1, event: "message", data: JSON.stringify({ id: 1, method: "read" }) });
  await tick();
  const sends = popup.posted.map(([m, o]) => [JSON.parse(m.data), o]);
  assert.deepEqual(sends, [
    [{ type: "hello", file: "a.scad" }, "http://127.0.0.1:6"],
    [{ id: 1, result: { ok: 1 } }, "http://127.0.0.1:6"],
  ]);
  c.disconnect();
  assert.equal(popup.closed, true);
  assert.equal(listeners.length, 0);
});

test("a blocked popup says so", () => {
  const c = new AgentConnection({ handle: async () => ({}), win: { open: () => null } });
  c.connectRelay({ port: 6, token: TOKEN });
  assert.deepEqual([c.state.status, c.state.reason], ["failed", "popup-blocked"]);
});

test("a loopback permission the browser already denied skips the direct attempt", async () => {
  // A stand-in for navigator.permissions that knows only `known` (others
  // throw, as Firefox and Safari do for a name they do not know).
  const perms = (known) => ({
    asked: [],
    async query({ name }) {
      this.asked.push(name);
      if (!(name in known)) throw new TypeError(`'${name}' is not a valid permission name`);
      return { state: known[name] };
    },
  });
  // Chrome 145 and later: the loopback permission decides.
  assert.equal(await loopbackDenied(perms({ "loopback-network": "denied" })), true);
  assert.equal(await loopbackDenied(perms({ "loopback-network": "prompt", "local-network-access": "denied" })), false);
  // Chrome 142-144 knows only the older name.
  const old = perms({ "local-network-access": "denied" });
  assert.equal(await loopbackDenied(old), true);
  assert.deepEqual(old.asked, ["loopback-network", "local-network-access"]);
  assert.equal(await loopbackDenied(perms({ "local-network-access": "granted" })), false);
  // Firefox and Safari: neither name, so try directly.
  assert.equal(await loopbackDenied(perms({})), false);
  // No permissions API at all (or a broken one).
  assert.equal(await loopbackDenied(undefined), false);
  assert.equal(await loopbackDenied({ query: async () => null }), false);
});

test("Chrome and Edge are told how to allow the direct way next time", () => {
  const chrome = (v) => `Mozilla/5.0 (Macintosh) AppleWebKit/537.36 Chrome/${v}.0.0.0 Safari/537.36`;
  assert.match(allowDirectHint(chrome(150)), /Site settings, and set “Apps on device” to Allow/);
  assert.match(allowDirectHint(chrome(143)), /“Local network access”/);
  assert.match(allowDirectHint(`${chrome(150)} Edg/150.0.0.0`), /“Apps on device”/);
  assert.equal(allowDirectHint("Mozilla/5.0 (Macintosh) Gecko/20100101 Firefox/155.0"), null);
  assert.equal(allowDirectHint("Mozilla/5.0 AppleWebKit/605.1.15 Version/27.0 Safari/605.1.15"), null);
  assert.equal(allowDirectHint(""), null);
});

test("read sends the page's text, part() switch and extensions", () => {
  // The agent's model tools run the page's text as it previews: with its
  // customizer values, its part() switch, and the NeoSCAD extensions
  // App.extensions() gives (the View menu's and a link's), which
  // `neoscad mcp` adds to its own --enable.
  const app = {
    doc: {
      example: { file: "box.scad" },
      path: "/box.scad",
      text: "cube(1);\n",
      parts: true,
      customizer: { values: { size: 7 } },
    },
    revision: 4,
    activeTab: null,
    editor: { selectionPositions: () => ({ anchor: [0, 0], head: [0, 0] }) },
    console: { lines: [{ kind: "warning", text: "w" }, { kind: "echo", text: "e" }], summaryText: "ok", summaryKind: "ok" },
    lastMode: "preview",
    extensions: () => ["sketch", "query"],
  };
  const r = new PageAgent(app, {}).read();
  assert.equal(r.text, "cube(1);\n");
  assert.equal(r.version, 4);
  assert.equal(r.parts, true);
  assert.deepEqual(r.values, { size: 7 });
  assert.deepEqual(r.enable, ["sketch", "query"]);
  assert.equal(r.diagnostics.length, 1);
  // None on: an empty list, not a missing field (a missing one is an
  // older page, which the server reads as none too).
  app.extensions = () => [];
  assert.deepEqual(new PageAgent(app, {}).read().enable, []);
});
