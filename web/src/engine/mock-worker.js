// The mock engine's worker: MockCore behind the worker envelope
// (docs/web-protocol.md), posting `ready` and, when a run "traps", the
// `crashed` reply and message the real glue (crates/web/js/worker.js)
// sends. The build uses it when no wasm core was built
// (scripts/web/build.sh), and the page says so in a banner.

import { MockCore, MockCrash, MOCK_VERSION } from "./mock-core.js";

const core = new MockCore();
let crashed = false;

self.addEventListener("message", (e) => {
  const msg = e.data;
  if (crashed) {
    self.postMessage({ id: msg.id, ok: false, error: { kind: "crashed", message: "the engine crashed; respawn the worker" } });
    return;
  }
  try {
    const out = core.handle(msg);
    self.postMessage({ id: msg.id, ok: true, result: out.result }, out.transfer ?? []);
  } catch (err) {
    if (err instanceof MockCrash) {
      // A trapped wasm instance can answer nothing more: say so, and the
      // client terminates this worker and starts another.
      crashed = true;
      self.postMessage({ id: msg.id, ok: false, error: { kind: "crashed", message: err.message } });
      self.postMessage({ type: "crashed", message: err.message });
      return;
    }
    self.postMessage({ id: msg.id, ok: false, error: { kind: err.kind ?? "failed", message: err.message } });
  }
});

self.postMessage({ type: "ready", version: MOCK_VERSION });
