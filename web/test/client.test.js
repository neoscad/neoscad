// The engine client: requests and answers, coalesced runs, and the
// respawn that follows a cancel, a crash or a stale run, with the new
// worker given back what the old one had.

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
    now: time.now,
    timers: time,
  });
  const statuses = [];
  c.on("status", (s) => statuses.push(s.state));
  return { c, workers, time, statuses };
}

const path = docPath("a.scad");

test("init, open and run answer through the envelope", async () => {
  const { c, statuses } = client();
  const info = await c.start();
  assert.equal(info.engine, "mock");
  await c.open(path, 'echo("hi");');
  const r = await c.run(Requests.run(path, "preview", [], false));
  assert.equal(r.render.exit_code, 0);
  assert.ok(r.console.some((l) => l.text === 'ECHO: "hi"'));
  assert.ok(r.scene.meshes.length >= 1);
  assert.deepEqual(statuses.slice(0, 2), ["working", "idle"]);
});

test("a failed request rejects with the worker's message", async () => {
  const { c } = client();
  await c.start();
  await assert.rejects(c.request(Requests.read("/nope")), (e) => e instanceof EngineError && /no such file/.test(e.message));
});

test("a run that waits is replaced by a newer one", async () => {
  const { c, workers } = client({ hold: true });
  await c.start();
  await c.open(path, "cube(1);");
  const first = c.run(Requests.run(path, "preview", [], false));
  await settle();
  const second = c.run(Requests.run(path, "preview", [{ name: "a", value: 1 }], false));
  const third = c.run(Requests.run(path, "render", [{ name: "a", value: 2 }], false));
  assert.deepEqual(await second, { superseded: true });
  workers[0].release();
  const r1 = await first;
  assert.equal(r1.render.exit_code, 0);
  await settle();
  workers[0].release();
  const r3 = await third;
  assert.ok(r3.render.geometry, "the render ran");
  const runs = workers[0].received.filter((m) => m.type === "run");
  assert.equal(runs.length, 2, "the replaced run never reached the worker");
  assert.deepEqual(runs[1].overrides, [{ name: "a", value: 2 }]);
});

test("a long run with a newer one waiting is abandoned by a respawn", async () => {
  const { c, workers, time, statuses } = client({ hold: true, staleMs: 1000 });
  await c.start();
  await c.open(path, "cube(1);");
  const first = c.run(Requests.run(path, "preview", [], false));
  await settle();
  time.advance(400);
  const second = c.run(Requests.run(path, "preview", [], false));
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
  assert.equal(workers[1].received[1].text, "cube(1);");
  workers[1].release();
  const r = await second;
  assert.equal(r.render.exit_code, 0);
});

test("cancel terminates a busy worker and the next run goes to a new one", async () => {
  const { c, workers } = client({ hold: true });
  await c.start();
  await c.open(path, "cube(1);");
  const run = c.run(Requests.run(path, "render", [], false));
  await settle();
  await c.cancel();
  assert.ok(workers[0].terminated);
  assert.deepEqual(await run, { superseded: true, restarted: true });
  assert.equal(workers.length, 2);
});

test("a crash (a trapped instance) respawns and replays files and documents", async () => {
  const { c, workers, statuses } = client();
  await c.start();
  await c.addFiles([{ path: "/neoscad/libraries/L/x.scad", text: "module x() {}" }]);
  await c.open(path, "cube(1); // mock:crash");
  const r = await c.run(Requests.run(path, "preview", [], false));
  assert.deepEqual(r, { superseded: true, restarted: true });
  await c.ready;
  assert.equal(workers.length, 2);
  assert.ok(statuses.includes("restarted"));
  const replay = workers[1].received.map((m) => m.type);
  assert.deepEqual(replay, ["init", "addFiles", "open"]);
  assert.equal(workers[1].core.files.get("/neoscad/libraries/L/x.scad"), "module x() {}");
});

test("a worker that keeps dying stops being respawned", async () => {
  const { c, workers, statuses } = client();
  await c.start();
  for (let i = 0; i < 6; i++) c.restart("boom");
  assert.ok(workers.length <= 6);
  assert.equal(statuses.at(-1), "failed");
  await assert.rejects(c.request(Requests.parameters(path)), EngineRestarted);
});

test("language-server messages go both ways without ids", async () => {
  const { c, workers } = client();
  await c.start();
  const got = [];
  c.on("lsp", (m) => got.push(JSON.parse(m)));
  c.lsp(JSON.stringify({ jsonrpc: "2.0", id: 7, method: "initialize", params: {} }));
  await settle();
  assert.equal(workers[0].received.at(-1).id, undefined);
  assert.equal(got[0].id, 7);
  assert.equal(got[0].result.capabilities.positionEncoding, "utf-16");
});
