// The engine client: requests and answers in the protocol's envelope
// (docs/web-protocol.md), coalesced runs, and the respawn that follows a
// cancel, a crash or a stale run, with the new worker given back what the
// old one had.

import assert from "node:assert/strict";
import { test } from "node:test";
import { EngineClient, EngineError, EngineRestarted } from "../src/engine/client.js";
import { Requests, docPath } from "../src/engine/protocol.js";
import { FakeTime, FakeWorker, settle } from "./helpers.js";

function client({ hold = false, staleMs = 3000 } = {}) {
  const workers = [];
  const time = new FakeTime();
  const c = new EngineClient({
    spawn: () => {
      const w = new FakeWorker({ hold });
      workers.push(w);
      return w;
    },
    staleMs,
    seed: 42,
    now: time.now,
    timers: time,
  });
  const statuses = [];
  c.on("status", (s) => statuses.push(s.state));
  return { c, workers, time, statuses };
}

const path = docPath("a.scad");
const run = (mode = "preview", values = {}) => Requests.run({ path, mode, values });

test("init, open and run answer through the envelope", async () => {
  const { c, workers, statuses } = client();
  const info = await c.start();
  assert.equal(info.version, "0.0.0-mock");
  assert.deepEqual(workers[0].received[0], { id: 1, type: "init", seed: 42 });
  assert.equal(c.defaults.checkOptions.nozzle, 0.4, "the worker's defaults are fetched once");
  await c.open(path, 'echo("hi");');
  const r = await c.run(run());
  assert.equal(r.render.exitCode, 0);
  assert.ok(r.console.some((l) => l.text === 'ECHO: "hi"'));
  assert.ok(r.scene.faces instanceof ArrayBuffer && r.scene.faces.byteLength % 44 === 0);
  assert.equal(typeof r.scene.meta, "string");
  assert.deepEqual(statuses.slice(0, 2), ["working", "idle"]);
});

test("a failed request rejects with the worker's kind and message", async () => {
  const { c } = client();
  await c.start();
  await assert.rejects(
    c.request(Requests.readFile("/nope")),
    (e) => e instanceof EngineError && e.kind === "failed" && /no such file/.test(e.message),
  );
  await assert.rejects(c.request({ type: "cancel" }), (e) => e.kind === "invalidArgument");
});

test("editor edits go out as LSP positions against the client's copy", async () => {
  const { c, workers } = client();
  await c.start();
  await c.open(path, "cube(1);\nsphere(2);");
  // The bridge's order: each edit applies to the text the previous left.
  await c.edit(path, [[16, 17, "5"], [0, 4, "cylinder"]]);
  const sent = workers[0].received.at(-1);
  assert.deepEqual(sent.edits, [
    { start: { line: 1, character: 7 }, end: { line: 1, character: 8 }, text: "5" },
    { start: { line: 0, character: 0 }, end: { line: 0, character: 4 }, text: "cylinder" },
  ]);
  assert.equal(workers[0].core.files.get(path), "cylinder(1);\nsphere(5);");
  assert.equal(c.documents.get(path), "cylinder(1);\nsphere(5);");
});

test("a run that waits is replaced by a newer one", async () => {
  const { c, workers } = client({ hold: true });
  await c.start();
  await c.open(path, "cube(1);");
  const first = c.run(run());
  await settle();
  const second = c.run(run("preview", { a: 1 }));
  const third = c.run(run("render", { a: 2 }));
  assert.deepEqual(await second, { superseded: true });
  workers[0].release();
  const r1 = await first;
  assert.equal(r1.render.exitCode, 0);
  await settle();
  workers[0].release();
  const r3 = await third;
  assert.ok(r3.render.geometry, "the render ran");
  const runs = workers[0].received.filter((m) => m.type === "run");
  assert.equal(runs.length, 2, "the replaced run never reached the worker");
  assert.deepEqual(runs[1].overrides, [{ name: "a", value: { kind: "number", value: 2 } }]);
});

test("a long run with a newer one waiting is abandoned by a respawn", async () => {
  const { c, workers, time, statuses } = client({ hold: true, staleMs: 1000 });
  await c.start();
  await c.open(path, "cube(1);");
  const first = c.run(run());
  await settle();
  time.advance(400);
  const second = c.run(run());
  time.advance(599);
  assert.equal(workers.length, 1, "not yet stale");
  time.advance(1);
  assert.equal(workers.length, 2, "respawned");
  assert.ok(workers[0].terminated);
  assert.ok(statuses.includes("restarted"));
  assert.deepEqual(await first, { superseded: true, restarted: true });
  await settle(10);
  // The new worker was given the document before the waiting run.
  const types = workers[1].received.map((m) => m.type);
  assert.deepEqual(types.slice(0, 3), ["init", "open", "run"]);
  assert.equal(workers[1].received.find((m) => m.type === "open").text, "cube(1);");
  workers[1].release();
  const r = await second;
  assert.equal(r.render.exitCode, 0);
});

test("cancel terminates a busy worker and the next run goes to a new one", async () => {
  const { c, workers } = client({ hold: true });
  await c.start();
  await c.open(path, "cube(1);");
  const pending = c.run(run("render"));
  await settle();
  await c.cancel();
  assert.ok(workers[0].terminated);
  assert.deepEqual(await pending, { superseded: true, restarted: true });
  assert.equal(workers.length, 2);
  assert.equal(workers[1].received[0].seed, 42, "the same seed after a respawn");
});

test("a crash respawns and replays files, documents and the language server", async () => {
  const { c, workers, statuses } = client();
  await c.start();
  const tar = new ArrayBuffer(1024); // an empty archive: two zero blocks
  await c.addFiles({ files: [{ path: "/neoscad/libraries/L/x.scad", data: "module x() {}" }] });
  await c.addFiles({ tar, root: "/neoscad/libraries" }, "BOSL2");
  const uri = "file:///doc/a.scad";
  c.lsp(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} }));
  c.lsp(JSON.stringify({ jsonrpc: "2.0", method: "initialized", params: {} }));
  c.lsp(
    JSON.stringify({
      jsonrpc: "2.0",
      method: "textDocument/didOpen",
      params: { textDocument: { uri, languageId: "openscad", version: 1, text: "cube(1);" } },
    }),
  );
  c.lsp(
    JSON.stringify({
      jsonrpc: "2.0",
      method: "textDocument/didChange",
      params: {
        textDocument: { uri, version: 2 },
        contentChanges: [{ range: { start: { line: 0, character: 5 }, end: { line: 0, character: 6 } }, text: "3" }],
      },
    }),
  );
  await c.open(path, "cube(1); // mock:crash");
  const r = await c.run(run());
  assert.deepEqual(r, { superseded: true, restarted: true });
  await c.ready;
  assert.equal(workers.length, 2);
  assert.ok(statuses.includes("restarted"));
  assert.ok(c.hasLibrary("BOSL2"));
  const replay = workers[1].received.map((m) => (m.type === "lsp" ? JSON.parse(m.message).method : m.type));
  assert.deepEqual(replay, ["init", "addFiles", "addFiles", "open", "initialize", "initialized", "textDocument/didOpen"]);
  assert.equal(workers[1].core.files.get("/neoscad/libraries/L/x.scad"), "module x() {}");
  const reopen = JSON.parse(workers[1].received.at(-1).message).params.textDocument;
  assert.deepEqual(reopen, { uri, languageId: "openscad", version: 2, text: "cube(3);" });
});

test("a worker that keeps dying stops being respawned", async () => {
  const { c, workers, statuses } = client();
  await c.start();
  for (let i = 0; i < 6; i++) c.restart("boom");
  assert.ok(workers.length <= 6);
  assert.equal(statuses.at(-1), "failed");
  await assert.rejects(c.request(Requests.parameters(path)), EngineRestarted);
});

test("language-server messages are requests; replies come back as events", async () => {
  const { c, workers, statuses } = client();
  // The editor's client starts before the engine: its messages wait.
  const got = [];
  c.on("lsp", (m) => got.push(JSON.parse(m)));
  c.lsp(JSON.stringify({ jsonrpc: "2.0", id: 7, method: "initialize", params: {} }));
  await c.start();
  const before = statuses.length;
  await settle(10);
  const sent = workers[0].received.find((m) => m.type === "lsp");
  assert.equal(JSON.parse(sent.message).id, 7);
  assert.equal(got[0].id, 7);
  assert.equal(got[0].result.capabilities.positionEncoding, "utf-16");
  assert.ok(!statuses.slice(before).includes("working"), "language requests do not show as work");
});

test("the page's language extensions go with every language-server message, and its replay", async () => {
  const { c, workers } = client();
  await c.start();
  c.lsp(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} }));
  await settle(10);
  assert.deepEqual(workers[0].received.at(-1).enable, [], "none until the page says");
  c.setExtensions(["sketch", "fillet"]);
  c.lsp(JSON.stringify({ jsonrpc: "2.0", id: 2, method: "textDocument/completion", params: {} }));
  await settle(10);
  assert.deepEqual(workers[0].received.at(-1).enable, ["sketch", "fillet"]);
  // A respawned worker's server is told them with the replayed state.
  c.restart("boom");
  await c.ready;
  await settle(10);
  const replayed = workers[1].received.filter((m) => m.type === "lsp");
  assert.ok(replayed.length > 0);
  for (const m of replayed) assert.deepEqual(m.enable, ["sketch", "fillet"]);
});
