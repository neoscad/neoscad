// The package's engine against the demo's mock core (web/src/engine/
// mock-core.js, which speaks the real wire) in an in-process worker:
// results, documents and files, and the per-model queue (scheduler.js)
// with slow runs, aborts and crashes.

import assert from "node:assert/strict";
import { test } from "node:test";
import { EngineRestarted } from "../../src/engine/client.js";
import { FakeTime, FakeWorker, settle } from "../../test/helpers.js";
import { AbortError, EngineStartError, ExportError, createEngine, checkRelativePath } from "../src/index.js";
import { EPOCH, splitEnable } from "../src/engine.js";
import { Scheduler } from "../src/scheduler.js";

function engine({ hold = false, ...options } = {}) {
  const workers = [];
  const e = createEngine({
    worker: () => {
      const w = new FakeWorker({ hold });
      workers.push(w);
      return w;
    },
    lazyFonts: false,
    ...options,
  });
  return { e, workers, sent: (type) => workers.flatMap((w) => w.received.filter((m) => m.type === type)) };
}

test("a run resolves to the package's result shape", async () => {
  const { e, workers } = engine();
  const info = await e.ready;
  assert.equal(info.version, "0.0.0-mock");
  assert.equal(e.version, "0.0.0-mock");
  assert.deepEqual(workers[0].received[0], { id: 1, type: "init", seed: 0 }, "seed 0 by default: the same model every load");
  const r = await e.run('echo("hi");\ncube(sz = nope);\n');
  assert.equal(r.ok, true);
  assert.equal(r.exitCode, 0);
  assert.ok(r.scene.faces instanceof ArrayBuffer);
  assert.deepEqual(Object.keys(r.bbox), ["min", "max"]);
  const echo = r.console.find((l) => l.kind === "echo");
  assert.deepEqual(echo.location, { path: "main.scad", line: 0, column: 0, endLine: 0, endColumn: 11 });
  assert.equal(r.warnings.length, 1);
  assert.match(r.warnings[0].message, /unknown variable "nope"/);
  assert.equal(r.warnings[0].line, 1);
  assert.equal(r.warnings[0].path, "main.scad");
  e.dispose();
});

test("limits go to init; overrides, enable and the colour scheme to the run", async () => {
  const limits = { timeSeconds: 3 };
  const { e, sent } = engine({ limits, seed: 9 });
  await e.run("w = 1; cube(w);", { overrides: { w: 20, on: true, name: "x", v: [1, 2] }, enable: "sketch part", colorScheme: "Metallic", mode: "render" });
  assert.deepEqual(sent("init")[0], { id: 1, type: "init", seed: 9, limits });
  const run = sent("run")[0];
  assert.equal(run.mode, "render");
  assert.equal(run.parts, true);
  assert.deepEqual(run.enable, ["sketch"]);
  assert.equal(run.colorScheme, "Metallic");
  assert.deepEqual(run.overrides, [
    { name: "name", value: { kind: "text", value: "x" } },
    { name: "on", value: { kind: "bool", value: true } },
    { name: "v", value: { kind: "vector", value: [1, 2] } },
    { name: "w", value: { kind: "number", value: 20 } },
  ]);
  e.dispose();
});

test("a model's document and files live in its own directory and are sent only when they change", async () => {
  const { e, sent } = engine();
  const a = e.model({ name: "bracket.scad" });
  const b = e.model();
  const files = { "lib/part.scad": "module part() cube(1);" };
  await a.run("include <lib/part.scad>\npart();", { files });
  await a.run("include <lib/part.scad>\npart();", { files });
  await b.run("cube(2);");
  assert.deepEqual(
    sent("addFiles").map((m) => m.files.map((f) => f.path)),
    [["/doc/m1/lib/part.scad"]],
    "an unchanged file is not sent again",
  );
  assert.deepEqual(sent("open").map((m) => m.path), ["/doc/m1/bracket.scad", "/doc/m2/main.scad"]);
  assert.equal(sent("update").length, 0, "the same text twice needs no update");
  await a.run("include <lib/part.scad>\npart(); cube(3);", { files: { "lib/part.scad": "module part() cube(2);" } });
  assert.equal(sent("addFiles").length, 2);
  assert.deepEqual(sent("update").map((m) => m.path), ["/doc/m1/bracket.scad"]);
  a.dispose();
  await settle(10);
  assert.deepEqual(sent("close").map((m) => m.path), ["/doc/m1/bracket.scad"]);
  e.dispose();
});

test("paths outside the model's directory are refused", () => {
  for (const p of ["../x.scad", "/abs.scad", "a//b.scad", "a/./b", "", "a\\b"]) assert.throws(() => checkRelativePath(p), TypeError, p);
  assert.equal(checkRelativePath("lib/a.scad"), "lib/a.scad");
  assert.deepEqual(splitEnable("part, sketch  fillet"), { parts: true, enable: ["sketch", "fillet"] });
});

test("runs are latest-wins per model and first-in first-out across models", async () => {
  const { e, workers } = engine({ hold: true });
  await e.ready;
  const a = e.model();
  const b = e.model();
  const a1 = a.run("cube(1);");
  await settle();
  const b1 = b.run("cube(2);");
  const a2 = a.run("cube(3);");
  const a3 = a.run("cube(4);");
  await settle();
  assert.deepEqual(await a2, { superseded: true }, "replaced while it waited");
  const order = [];
  for (let i = 0; i < 6 && order.length < 3; i++) {
    for (const m of workers[0].held) order.push(m.path);
    workers[0].release();
    await settle(10);
  }
  assert.deepEqual(order, ["/doc/m1/main.scad", "/doc/m2/main.scad", "/doc/m1/main.scad"]);
  assert.equal((await a1).ok, true);
  assert.equal((await b1).ok, true);
  assert.equal((await a3).ok, true);
  assert.equal(workers.length, 1, "nothing restarted the worker");
  e.dispose();
});

test("aborting a waiting run drops it; aborting the one in flight restarts the worker and the others go on", async () => {
  const { e, workers, sent } = engine({ hold: true });
  await e.ready;
  const a = e.model();
  const b = e.model();
  const slow = new AbortController();
  const waiting = new AbortController();
  const a1 = a.run("cube(1);", { signal: slow.signal });
  await settle();
  const b1 = b.run("cube(2);", { signal: waiting.signal });
  const b2 = e.model().run("cube(5);");
  waiting.abort();
  await assert.rejects(b1, AbortError);
  slow.abort();
  await assert.rejects(a1, AbortError);
  assert.equal(workers.length, 2, "the run in flight could only be stopped by a new worker");
  await settle(20);
  workers[1].release();
  const r = await b2;
  assert.equal(r.ok, true);
  assert.equal(sent("run").filter((m) => m.path === "/doc/m2/main.scad").length, 0, "the aborted waiting run never went out");
  assert.ok(workers[1].received.some((m) => m.type === "open" && m.path === "/doc/m1/main.scad"), "the new worker has the documents back");
  e.dispose();
});

test("a crash fails that model's run, and other models carry on", async () => {
  const { e, workers } = engine();
  const bad = e.model().run("// mock:crash\ncube(1);");
  const good = e.model().run("cube(2);");
  await assert.rejects(bad, (err) => err instanceof EngineRestarted && /crashed/.test(err.message));
  assert.equal((await good).ok, true);
  assert.equal(workers.length, 2);
  e.dispose();
});

test("a stale run of a model is abandoned only for a newer run of the same model", async () => {
  const time = new FakeTime();
  let restarts = 0;
  const client = {
    restart: async () => {
      restarts += 1;
      for (const r of pending.splice(0)) r(new EngineRestarted("a newer run replaced a long one"));
    },
  };
  const pending = [];
  const s = new Scheduler(client, { staleMs: 1000, timers: time, now: time.now });
  const job = () => new Promise((_, reject) => pending.push(reject));
  const fast = () => Promise.resolve("done");
  const a1 = s.submit("a", job);
  s.submit("b", fast);
  await settle();
  time.advance(5000);
  assert.equal(restarts, 0, "another model waiting does not make a run stale");
  const a2 = s.submit("a", fast);
  await settle();
  time.advance(0);
  assert.equal(restarts, 1);
  assert.deepEqual(await a1, { superseded: true });
  assert.equal(await a2, "done");
});

test("exports return bytes, use the fixed date, and throw ExportError without a file", async () => {
  const { e, sent } = engine();
  const r = await e.export("cube(10);", "stl");
  assert.ok(r.data instanceof Uint8Array && r.data.length > 0);
  assert.equal(r.ok, true);
  assert.equal(r.mime, "model/stl");
  assert.equal(sent("export")[0].creationDate, EPOCH);
  await e.export("cube(10);", "3mf", { creationDate: "2026-01-02T03:04:05Z" });
  assert.equal(sent("export")[1].creationDate, "2026-01-02T03:04:05Z");
  await assert.rejects(e.export("cube(1);", "step"), (err) => err instanceof ExportError && /exact/.test(err.failure));
  e.dispose();
});

test("parameters come back as customizer groups", async () => {
  const { e } = engine();
  const groups = await e.parameters("/* [Size] */\nwidth = 20; // [10:50]\n");
  assert.equal(groups[0].name, "Size");
  assert.equal(groups[0].parameters[0].name, "width");
  assert.equal(groups[0].parameters[0].defaultValue, 20);
  e.dispose();
});

test("a worker that never starts gives a message naming the assets and CORS/CSP", async () => {
  class Dead {
    addEventListener(type, fn) {
      if (type === "error") setImmediate(() => fn({ message: "load failed" }));
    }
    postMessage() {}
    terminate() {}
  }
  const e = createEngine({ worker: () => new Dead(), assetsUrl: "https://cdn.example/neoscad/assets/" });
  await assert.rejects(e.ready, (err) => err instanceof EngineStartError && /cdn\.example\/neoscad\/assets\/core\/worker\.js/.test(err.message) && /Content-Security-Policy/.test(err.message));
  await assert.rejects(e.run("cube(1);"), EngineStartError);
  e.dispose();
});

test("a run after dispose is refused", async () => {
  const { e } = engine();
  const m = e.model();
  m.dispose();
  await assert.rejects(m.run("cube(1);"), /disposed/);
  await assert.rejects(e.run("cube(1);", { mode: "bogus" }), TypeError);
  e.dispose();
});

test("queued one-off runs keep their own file names", async () => {
  const { e, workers, sent } = engine({ hold: true });
  await e.ready;
  const a = e.run("cube(1);", { name: "a.scad" });
  const b = e.run("cube(2);", { name: "b.scad" });
  for (let i = 0; i < 6; i++) {
    workers[0].release();
    await settle(10);
  }
  await Promise.all([a, b]);
  assert.deepEqual(sent("run").map((m) => m.path), ["/doc/run/a.scad", "/doc/run/b.scad"]);
  e.dispose();
});
