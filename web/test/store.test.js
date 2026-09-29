// Persistence: edited examples, customizer values and settings in
// localStorage, and a page that keeps working when storage fails.

import assert from "node:assert/strict";
import { test } from "node:test";
import { MemoryStorage, Store } from "../src/store.js";

test("an edited example is kept; the shipped text is not", () => {
  const s = new Store(new MemoryStorage());
  assert.equal(s.exampleText("csg"), null);
  s.setExampleText("csg", "cube(2);", "cube(1);");
  assert.equal(s.exampleText("csg"), "cube(2);");
  // Edited back to the original: unedited again.
  s.setExampleText("csg", "cube(1);", "cube(1);");
  assert.equal(s.exampleText("csg"), null);
});

test("reset forgets the text and the customizer values", () => {
  const storage = new MemoryStorage();
  const s = new Store(storage);
  s.setExampleText("sign", "x", "y");
  s.setParameterValues("sign", { radius: 100 });
  s.setParameterValues("other", { a: 1 });
  assert.deepEqual(s.parameterValues("sign"), { radius: 100 });
  s.resetExample("sign");
  assert.equal(s.exampleText("sign"), null);
  assert.deepEqual(s.parameterValues("sign"), {});
  assert.deepEqual(s.parameterValues("other"), { a: 1 });
  // No values: no key left behind.
  s.setParameterValues("other", {});
  assert.equal(storage.length, 0);
});

test("settings merge over the defaults and ignore junk", () => {
  const storage = new MemoryStorage();
  const s = new Store(storage);
  assert.deepEqual(s.settings({ a: 1, b: 2 }), { a: 1, b: 2 });
  s.setSettings({ b: 3 });
  assert.deepEqual(s.settings({ a: 1, b: 2 }), { a: 1, b: 3 });
  storage.setItem("neoscad.try.v1.settings", "{not json");
  assert.deepEqual(s.settings({ a: 1 }), { a: 1 });
  storage.setItem("neoscad.try.v1.example.x.params", "[1,2]");
  assert.deepEqual(s.parameterValues("x"), {});
});

test("a storage that throws (private mode, quota) is survivable", () => {
  const bad = {
    getItem() {
      throw new Error("denied");
    },
    setItem() {
      throw new Error("quota");
    },
    removeItem() {
      throw new Error("denied");
    },
  };
  const s = new Store(bad);
  assert.equal(s.exampleText("a"), null);
  assert.equal(s.setExampleText("a", "x", "y"), false);
  assert.deepEqual(s.settings({ a: 1 }), { a: 1 });
  const none = Store.fromWindow({
    get localStorage() {
      throw new Error("SecurityError");
    },
  });
  assert.equal(none.storage, null);
  assert.equal(none.set("k", 1), true);
});
