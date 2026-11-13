// Several scenes in one viewer (`addScene` and its handles) and the frame
// loop (`onFrame`), against a fake backend and a fake clock: the
// bookkeeping is the package's, the drawing the wasm viewer's (the e2e
// checks the pixels).

import assert from "node:assert/strict";
import { test } from "node:test";
import { FrameLoop, MAX_STEP_MS } from "../src/frames.js";
import { Viewer } from "../src/viewer.js";

/// The wasm viewer's wrapper (web/src/view/index.js), recording calls.
function fakeInner({ transforms = true } = {}) {
  const calls = [];
  let next = 1;
  const shown = new Set();
  return {
    calls,
    shown,
    kind: transforms ? "webgpu" : "canvas2d",
    supportsTransforms: transforms || undefined,
    setScene(scene) {
      calls.push(["setScene", scene]);
      shown.clear();
    },
    addScene(scene) {
      const id = next++;
      shown.add(id);
      calls.push(["addScene", id]);
      return id;
    },
    setSceneMatrix(id, m) {
      calls.push(["setSceneMatrix", id, Array.from(m), m]);
      return shown.has(id);
    },
    removeScene(id) {
      calls.push(["removeScene", id]);
      return shown.delete(id);
    },
    dispose() {
      calls.push(["dispose"]);
    },
  };
}

const viewerOn = (inner) => new Viewer(inner, {}, { notice: null, errors: [], made: false, settings: {} });
const SCENE = { faces: new ArrayBuffer(0), edges: new ArrayBuffer(0), meta: "{}" };
const moveX = (x) => [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, x, 0, 0, 1];

test("added scenes get handles that move and remove them; setScene ends them", () => {
  const inner = fakeInner();
  const v = viewerOn(inner);
  assert.equal(v.supportsTransforms, true);
  const a = v.addScene(SCENE);
  const b = v.addScene(SCENE, { matrix: moveX(5) });
  assert.notEqual(a.id, b.id);
  assert.deepEqual(inner.calls.at(-1).slice(0, 3), ["setSceneMatrix", b.id, moveX(5)]);

  // One scratch array per handle, reused: moving a scene every frame
  // allocates nothing.
  assert.equal(b.setMatrix(moveX(6)), true);
  const first = inner.calls.at(-1)[3];
  b.setMatrix(new Float32Array(moveX(7)));
  assert.equal(inner.calls.at(-1)[3], first);
  assert.ok(first instanceof Float32Array);
  assert.deepEqual(Array.from(first), moveX(7));
  // A DOMMatrix-like object is read through toFloat32Array().
  b.setMatrix({ toFloat32Array: () => new Float32Array(moveX(8)) });
  assert.deepEqual(inner.calls.at(-1)[2], moveX(8));
  assert.throws(() => b.setMatrix([1, 2, 3]), TypeError);

  assert.equal(a.remove(), true);
  assert.equal(a.remove(), false);
  assert.equal(a.setMatrix(moveX(1)), false);

  // setScene replaces every added scene: the handle finds out on its next move.
  v.setScene(SCENE);
  assert.equal(b.setMatrix(moveX(1)), false);
  assert.equal(b.removed, true);

  v.dispose();
  const c = new Viewer(fakeInner(), {}, { notice: null, errors: [], made: false, settings: {} });
  const h = c.addScene(SCENE);
  c.dispose();
  assert.equal(h.setMatrix(moveX(1)), false, "a disposed viewer's handles do nothing");
});

test("the canvas2d fallback says it cannot place scenes, and refuses", () => {
  const v = viewerOn(fakeInner({ transforms: false }));
  assert.equal(v.supportsTransforms, false);
  assert.throws(() => v.addScene(SCENE), /cannot show several scenes/);
  assert.throws(() => viewerOn(fakeInner()).addScene(null), TypeError);
});

/// A fake animation-frame clock, document and IntersectionObserver.
function fakeEnv() {
  let next = 1;
  const frames = new Map();
  const listeners = new Map();
  const env = {
    observed: [],
    document: {
      hidden: false,
      addEventListener: (t, f) => listeners.set(t, f),
      removeEventListener: (t) => listeners.delete(t),
    },
    requestAnimationFrame(f) {
      const id = next++;
      frames.set(id, f);
      return id;
    },
    cancelAnimationFrame: (id) => frames.delete(id),
    IntersectionObserver: class {
      constructor(cb) {
        this.cb = cb;
        env.observer = this;
      }
      observe(el) {
        env.observed.push(el);
      }
      disconnect() {
        env.observer = null;
      }
    },
    /// Run the pending frames at `now`.
    run(now) {
      const due = [...frames.values()];
      frames.clear();
      for (const f of due) f(now);
    },
    pending: () => frames.size,
    setHidden(h) {
      env.document.hidden = h;
      listeners.get("visibilitychange")?.();
    },
    setVisible(v) {
      env.observer?.cb([{ isIntersecting: v }]);
    },
    listening: () => listeners.has("visibilitychange"),
  };
  return env;
}

test("the frame loop counts running time only, and runs only on screen in a visible tab", () => {
  const env = fakeEnv();
  const seen = [];
  const loop = new FrameLoop("canvas", (t, dt) => seen.push([t, dt]), env);
  assert.deepEqual(env.observed, ["canvas"]);
  env.run(1000);
  env.run(1016);
  env.run(1032);
  assert.deepEqual(seen, [
    [0, 0],
    [16, 16],
    [32, 16],
  ]);

  // A hidden tab: no frames; back, the clock goes on from where it was.
  env.setHidden(true);
  assert.equal(loop.running, false);
  assert.equal(env.pending(), 0);
  env.setHidden(false);
  env.run(9000);
  env.run(9010);
  assert.deepEqual(seen.slice(-2), [
    [32, 0],
    [42, 10],
  ]);

  // Scrolled off screen: the same.
  env.setVisible(false);
  assert.equal(env.pending(), 0);
  env.setVisible(true);
  env.run(20000);
  assert.deepEqual(seen.at(-1), [42, 0]);

  // A long stall counts as one capped step.
  env.run(25000);
  assert.deepEqual(seen.at(-1), [42 + MAX_STEP_MS, MAX_STEP_MS]);

  // Paused by the page, and played again.
  loop.pause();
  assert.equal(loop.paused, true);
  assert.equal(env.pending(), 0);
  loop.play();
  assert.equal(loop.time, 42 + MAX_STEP_MS);
  assert.equal(env.pending(), 1);

  loop.stop();
  assert.equal(env.pending(), 0);
  assert.equal(env.listening(), false);
  assert.equal(env.observer, null);
  loop.play();
  assert.equal(env.pending(), 0, "a stopped loop stays stopped");
});

test("a callback that throws does not end the loop", () => {
  const env = fakeEnv();
  let n = 0;
  new FrameLoop(null, () => {
    n++;
    throw new Error("boom");
  }, env);
  assert.throws(() => env.run(0), /boom/);
  assert.equal(env.pending(), 1);
  assert.throws(() => env.run(16), /boom/);
  assert.equal(n, 2);
  assert.throws(() => new FrameLoop(null, null, env), TypeError);
});

test("dispose stops the viewer's frame loops", () => {
  const v = viewerOn(fakeInner());
  // No DOM here: the loop uses the global rAF, so give it one.
  const saved = [globalThis.requestAnimationFrame, globalThis.cancelAnimationFrame];
  let cancelled = 0;
  globalThis.requestAnimationFrame = () => 7;
  globalThis.cancelAnimationFrame = () => cancelled++;
  try {
    const loop = v.onFrame(() => {});
    assert.equal(loop.running, true);
    v.dispose();
    assert.equal(loop.running, false);
    assert.equal(cancelled, 1);
    assert.equal(v.loops.size, 0);
  } finally {
    [globalThis.requestAnimationFrame, globalThis.cancelAnimationFrame] = saved;
  }
});
