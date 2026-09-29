// The page's end of `neoscad mcp --browser` (crates/cli/src/mcp/bridge.rs):
// a WebSocket to the bridge on 127.0.0.1, directly or through the relay
// window, and the requests that come over it.
//
// Directly where the browser allows it: Firefox does; Chrome and Edge do
// once the user allows "local network access" when asked (and refuse if
// they say no); Safari never does from an https page (mixed content). The
// relay window works in all of them: it is a page of the bridge's own, so
// its socket is same-origin, and it passes messages here with
// postMessage. Opening it needs a click (a popup), which is why the page
// tries the direct way first and offers the window when that fails. See
// docs/agent-bridge.md for the browsers as tested.
//
// The protocol, both ways JSON text: the bridge sends `{type: "welcome",
// client, server}` and requests `{id, method, params}`; the page answers
// `{id, result}` or `{id, error: {message}}`, and says `{type: "hello",
// file, browser, page}` when it connects.

import { relayOrigin, relayURL, wsURL } from "./link.js";

/// The close code of a tab that another tab replaced (bridge.rs).
export const REPLACED = 4001;

export class AgentConnection {
  /// `handle(method, params)` answers a request (a value or a promise);
  /// `onChange(state)` hears every change of state; `hello()` says what
  /// the page is.
  constructor({ handle, onChange = () => {}, hello = () => ({}), WebSocketImpl = globalThis.WebSocket, win = globalThis.window }) {
    this.handle = handle;
    this.onChange = onChange;
    this.hello = hello;
    this.WS = WebSocketImpl;
    this.win = win;
    this.gen = 0;
    this.ws = null;
    this.popup = null;
    this.link = null;
    this.state = { status: "idle", via: null, reason: null, client: null };
  }

  set(change) {
    Object.assign(this.state, change);
    this.onChange({ ...this.state });
  }

  get connected() {
    return this.state.status === "connected";
  }

  /// Connect straight to the bridge. Fails (state "failed", reason
  /// "direct") when the browser blocks it or the bridge is not there.
  connectDirect(link) {
    this.close();
    const gen = ++this.gen;
    this.link = link;
    this.set({ status: "connecting", via: "direct", reason: null, client: null });
    let ws;
    try {
      ws = new this.WS(wsURL(link));
    } catch {
      this.set({ status: "failed", reason: "direct" });
      return;
    }
    this.ws = ws;
    let opened = false;
    ws.onopen = () => {
      if (gen !== this.gen) return;
      opened = true;
      this.opened("direct");
    };
    ws.onmessage = (e) => gen === this.gen && this.receive(e.data);
    // WebKit reports a socket it blocked as mixed content with `error`
    // and never a `close` (Playwright's WebKit 26.6, docs/agent-bridge.md):
    // waiting for `close` left the page "connecting" for ever.
    ws.onerror = () => {
      if (gen !== this.gen || opened) return;
      this.ws = null;
      this.set({ status: "failed", reason: "direct" });
    };
    ws.onclose = (e) => {
      if (gen !== this.gen) return;
      this.ws = null;
      if (opened) this.closed(e.code, e.reason);
      else this.set({ status: "failed", reason: "direct" });
    };
  }

  /// Connect through the relay window. Call it from a click: it opens a
  /// popup.
  connectRelay(link) {
    this.close();
    const gen = ++this.gen;
    this.link = link;
    const popup = this.win.open(relayURL(link), "neoscad-agent-relay", "popup,width=460,height=300");
    if (!popup) {
      this.set({ status: "failed", via: "relay", reason: "popup-blocked" });
      return;
    }
    this.popup = popup;
    this.set({ status: "connecting", via: "relay", reason: null, client: null });
    let opened = false;
    const origin = relayOrigin(link);
    this.onRelay = (e) => {
      if (gen !== this.gen || e.source !== popup || e.origin !== origin || e.data?.neoscadRelay !== 1) return;
      const d = e.data;
      if (d.event === "open") {
        opened = true;
        this.opened("relay");
      } else if (d.event === "message") this.receive(d.data);
      else if (d.event === "close") {
        if (opened) this.closed(d.code, d.reason);
        else this.fail("relay");
      }
    };
    this.win.addEventListener("message", this.onRelay);
    this.watch = setInterval(() => {
      if (gen !== this.gen || !popup.closed) return;
      if (opened) this.closed(1000, "the connection window was closed");
      else this.fail("relay");
    }, 500);
  }

  fail(reason) {
    this.close();
    this.set({ status: "failed", reason });
  }

  opened(via) {
    this.set({ status: "connected", via, reason: null });
    this.send({ type: "hello", ...this.hello() });
  }

  closed(code, reason) {
    this.close();
    this.set({
      status: "closed",
      reason: code === REPLACED ? "replaced" : reason || "the agent's neoscad stopped",
    });
  }

  send(message) {
    const text = JSON.stringify(message);
    if (this.ws) this.ws.send(text);
    else if (this.popup && !this.popup.closed) {
      this.popup.postMessage({ neoscadRelay: 1, event: "send", data: text }, relayOrigin(this.link));
    }
  }

  async receive(data) {
    let m;
    try {
      m = JSON.parse(data);
    } catch {
      return;
    }
    if (m?.type === "welcome") {
      this.set({ client: m.client ?? null, server: m.server ?? null });
      return;
    }
    if (m?.id == null || typeof m.method !== "string") return;
    try {
      const result = await this.handle(m.method, m.params ?? {});
      this.send({ id: m.id, result: result ?? {} });
    } catch (e) {
      this.send({ id: m.id, error: { message: String(e?.message ?? e) } });
    }
  }

  /// End the connection from the page (the Disconnect button).
  disconnect() {
    this.close();
    this.set({ status: "idle", via: null, reason: null, client: null });
  }

  /// Drop the socket or the relay window, quietly: later events of the
  /// old connection are ignored (`gen`).
  close() {
    this.gen += 1;
    if (this.ws) {
      const ws = this.ws;
      this.ws = null;
      ws.onclose = ws.onmessage = ws.onopen = ws.onerror = null;
      try {
        ws.close(1000, "disconnected in the page");
      } catch {
        // Already closed.
      }
    }
    if (this.popup) {
      const popup = this.popup;
      this.popup = null;
      if (!popup.closed) {
        popup.postMessage({ neoscadRelay: 1, event: "close" }, relayOrigin(this.link));
        popup.close();
      }
    }
    clearInterval(this.watch);
    if (this.onRelay) this.win.removeEventListener("message", this.onRelay);
    this.onRelay = null;
  }
}
