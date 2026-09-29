// The mock engine's worker: MockCore behind the worker envelope
// (protocol.js). The build uses it when no wasm core was built
// (scripts/web/build.sh), and says so in a banner.

import { MockCore, MockCrash } from "./mock-core.js";

const core = new MockCore();
const notify = (msg) => self.postMessage(msg);

self.addEventListener("message", (e) => {
  const msg = e.data;
  try {
    const out = core.handle(msg, notify);
    if (msg.id == null || out === null) return;
    self.postMessage({ id: msg.id, ok: true, result: out.result }, out.transfer ?? []);
  } catch (err) {
    if (err instanceof MockCrash) {
      // A trapped wasm instance can answer nothing more: say so, and the
      // client terminates this worker and starts another.
      self.postMessage({ type: "fatal", message: err.message });
      return;
    }
    if (msg.id != null) self.postMessage({ id: msg.id, ok: false, error: { message: err.message } });
  }
});
