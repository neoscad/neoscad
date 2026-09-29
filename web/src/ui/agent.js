// "Connect your AI agent": the top bar's button and its dialog, the page's
// way in to `neoscad mcp --browser` (docs/agent-bridge.md). The
// dialog says what it does, how to set up each agent, and how to connect;
// it shows the live state with Disconnect, and the switch that makes each
// agent edit wait for the user's Apply.
//
// First visits get a gentle shimmer on the button (once per browser, and
// none with reduced motion) instead of a popup: the button is the whole
// invitation.

import { AgentConnection } from "../agent/connection.js";
import { linkText, parseConnect } from "../agent/link.js";
import { PageAgent } from "../agent/page.js";
import { clear, h } from "./dom.js";

/// The link of this tab's connection, kept for a reload (per tab: it is
/// sessionStorage, not shared with other tabs).
const SESSION_KEY = "neoscad.agent.link";
/// Answer an "ask before applying" question before the command line gives
/// up on the edit (its 150 s).
const CONFIRM_MS = 140000;

const SETUPS = [
  {
    id: "claude-code",
    label: "Claude Code",
    note: "In a terminal:",
    code: "claude mcp add neoscad -- neoscad mcp --browser",
  },
  {
    id: "claude-desktop",
    label: "Claude Desktop",
    note: "Settings → Developer → Edit Config, add this, and restart Claude. If it cannot start neoscad, give its full path (from `which neoscad`) as the command.",
    code: '{\n  "mcpServers": {\n    "neoscad": { "command": "neoscad", "args": ["mcp", "--browser"] }\n  }\n}',
  },
  {
    id: "cursor",
    label: "Cursor",
    note: "In .cursor/mcp.json of your project, or ~/.cursor/mcp.json for all of them:",
    code: '{\n  "mcpServers": {\n    "neoscad": { "command": "neoscad", "args": ["mcp", "--browser"] }\n  }\n}',
  },
  {
    id: "vscode",
    label: "VS Code",
    note: "In .vscode/mcp.json (or run code --add-mcp with the same fields):",
    code: '{\n  "servers": {\n    "neoscad": { "type": "stdio", "command": "neoscad", "args": ["mcp", "--browser"] }\n  }\n}',
  },
  {
    id: "other",
    label: "Other",
    note: "Any MCP client: run this as a stdio server.",
    code: "neoscad mcp --browser",
  },
];

export const IDEAS = [
  "Make the teeth smaller and show me the result.",
  "Why won't this print? Mark the problem spots in the view.",
  "Walk me through this model, pointing at each part in the 3D view.",
  "Turn the fixed sizes into customizer parameters.",
  "Add M3 mounting holes in each corner, then check it still prints.",
  "Make it lighter without any wall under 2 mm.",
];

const DOING = {
  read: "is reading the code",
  edit: "is editing",
  reveal: "is showing you the code",
  camera: "is moving the camera",
  capture: "is looking at the view",
  annotate: "is pointing at the model",
  console: "is reading the console",
};

/// Requests that change what the page shows.
const SHOWN = new Set(["edit", "reveal", "camera", "annotate"]);

export class AgentPanel {
  constructor(app) {
    this.app = app;
    this.store = app.store;
    this.askFirst = !!this.store.get("agent.askFirst", false);
    this.page = new PageAgent(app, this);
    this.conn = new AgentConnection({
      handle: (m, p) => this.page.handle(m, p),
      hello: () => this.page.hello(),
      onChange: (s) => this.changed(s),
    });
    this.setup = this.store.get("agent.setup", "claude-code");
    this.downloadHref = "https://neoscad.org/download.html";

    this.label = h("span", { class: "agent-label" }, "Connect your AI agent");
    this.button = h(
      "button",
      {
        class: "agent-button",
        type: "button",
        "aria-haspopup": "dialog",
        "data-testid": "agent-button",
        "data-state": "idle",
        onclick: () => this.open(),
      },
      h("span", { class: "agent-spark", "aria-hidden": "true" }, "✦"),
      this.label,
    );
    if (!this.store.get("agent.seen", false)) this.button.classList.add("fresh");

    this.status = h("div", { class: "agent-status", role: "status", "aria-live": "polite", "data-testid": "agent-status" });
    this.pasted = h("input", {
      type: "text",
      class: "agent-paste",
      placeholder: "Paste the link your agent gave you",
      "aria-label": "Connect link",
      autocomplete: "off",
      spellcheck: "false",
      "data-testid": "agent-paste",
    });
    this.setupBody = h("div", { class: "agent-setup-body" });
    this.setupTabs = h("div", { class: "agent-tabs", role: "tablist", "aria-label": "Agent" });
    this.dialog = this.buildDialog();
    this.confirmBar = h("div", { class: "agent-confirm", role: "alertdialog", "aria-live": "assertive", hidden: true, "data-testid": "agent-confirm" });
    document.body.append(this.dialog, this.confirmBar);
    this.renderSetup();
    this.changed(this.conn.state);
  }

  // --- The dialog -----------------------------------------------------------

  buildDialog() {
    const close = () => this.dialog.close();
    const ideas = IDEAS.map((idea) =>
      h(
        "li",
        {},
        h("button", { type: "button", class: "agent-idea", title: "Copy", onclick: (e) => copy(idea, e.currentTarget) }, `“${idea}”`),
      ),
    );
    const d = h(
      "dialog",
      { class: "agent-dialog", "aria-labelledby": "agent-title", "data-testid": "agent-dialog" },
      h(
        "div",
        { class: "agent-head" },
        h("h2", { id: "agent-title", class: "grad" }, "Connect your AI agent"),
        h("button", { type: "button", class: "icon-button agent-close", "aria-label": "Close", onclick: close }, "×"),
      ),
      h(
        "p",
        { class: "agent-lead" },
        "Let Claude Code, Claude Desktop, Cursor, VS Code or any MCP agent work on this model with you. " +
          "It reads and edits the code in this editor, sees the 3D view and points at things in it. " +
          "Each change it makes shows up highlighted here, and Undo takes it back.",
      ),
      h(
        "p",
        { class: "agent-phone note" },
        "Agents connect through the neoscad program on a desktop computer. Open neoscad.org/try there to try it; the steps are below.",
      ),
      this.status,
      h(
        "ol",
        { class: "agent-steps" },
        h(
          "li",
          {},
          h("h3", {}, "Install neoscad"),
          h("p", {}, "The command-line tool, for macOS, Linux and Windows: ", (this.download = h("a", { href: this.downloadHref, target: "_blank", rel: "noopener" }, "download NeoSCAD")), "."),
        ),
        h("li", {}, h("h3", {}, "Add it to your agent"), this.setupTabs, this.setupBody),
        h(
          "li",
          {},
          h("h3", {}, "Connect"),
          h("p", {}, "Ask your agent to ", h("em", {}, "“connect to my NeoSCAD page”"), ". It gives you a link: open it, or paste it here."),
          h(
            "form",
            {
              class: "agent-connect",
              onsubmit: (e) => {
                e.preventDefault();
                this.connectPasted();
              },
            },
            this.pasted,
            h("button", { type: "submit", class: "primary", "data-testid": "agent-connect" }, "Connect"),
          ),
        ),
      ),
      h("section", { class: "agent-ideas" }, h("h3", {}, "Things to ask"), h("ul", {}, ideas)),
      h(
        "label",
        { class: "agent-option" },
        h("input", {
          type: "checkbox",
          checked: this.askFirst,
          "data-testid": "agent-ask",
          onchange: (e) => {
            this.askFirst = e.target.checked;
            this.store.set("agent.askFirst", this.askFirst);
          },
        }),
        " Ask me before applying the agent's edits",
      ),
      h(
        "p",
        { class: "agent-fine muted" },
        "The agent reaches this tab through neoscad on your own computer (127.0.0.1), with a one-time key in the link; nothing passes through neoscad.org. " +
          "Desktop Chrome, Edge, Firefox and Safari.",
      ),
    );
    // A click on the backdrop (the dialog element itself, outside its box)
    // closes it.
    d.addEventListener("click", (e) => {
      if (e.target === d) close();
    });
    return d;
  }

  renderSetup() {
    clear(
      this.setupTabs,
      SETUPS.map((s) =>
        h(
          "button",
          {
            type: "button",
            role: "tab",
            "aria-selected": String(s.id === this.setup),
            onclick: () => {
              this.setup = s.id;
              this.store.set("agent.setup", s.id);
              this.renderSetup();
            },
          },
          s.label,
        ),
      ),
    );
    const s = SETUPS.find((x) => x.id === this.setup) ?? SETUPS[0];
    clear(
      this.setupBody,
      h("p", { class: "muted" }, s.note),
      h(
        "div",
        { class: "agent-code" },
        h("pre", {}, h("code", { "data-testid": "agent-setup-code" }, s.code)),
        h("button", { type: "button", class: "agent-copy", onclick: (e) => copy(s.code, e.currentTarget) }, "Copy"),
      ),
    );
  }

  setSite(site) {
    const nav = site.nav.find((n) => /download/i.test(n.label));
    this.downloadHref = nav?.href ?? new URL("/download.html", site.home).href;
    if (this.download) this.download.href = this.downloadHref;
  }

  open() {
    if (!this.dialog.open) this.dialog.showModal();
    if (this.button.classList.contains("fresh")) {
      this.button.classList.remove("fresh");
      this.store.set("agent.seen", true);
    }
  }

  // --- Connecting -------------------------------------------------------------

  /// Connect with a link (the page was opened with one, or a reload found
  /// this tab's last one). `quiet`: a reload's attempt, which does not open
  /// the dialog when it fails.
  connect(link, { quiet = false } = {}) {
    this.link = link;
    this.quiet = quiet;
    try {
      sessionStorage.setItem(SESSION_KEY, linkText(link));
    } catch {
      // Storage refused: a reload just forgets the link.
    }
    this.conn.connectDirect(link);
  }

  /// Skip the direct attempt and offer the connection window (a link with
  /// `&via=relay`): for a browser known to block the direct way, and for
  /// the tests, which serve the page from 127.0.0.1 where direct works.
  offerRelay(link) {
    this.link = link;
    this.quiet = false;
    this.conn.set({ status: "failed", via: "direct", reason: "direct" });
  }

  connectPasted() {
    const link = parseConnect(this.pasted.value);
    if (!link) {
      this.pasted.setCustomValidity("That is not a NeoSCAD connect link (it ends in #connect=PORT.KEY).");
      this.pasted.reportValidity();
      return;
    }
    this.pasted.setCustomValidity("");
    this.pasted.value = "";
    this.connect(link);
  }

  /// This tab's link from before a reload, if any.
  static saved() {
    try {
      return parseConnect(sessionStorage.getItem(SESSION_KEY) ?? "");
    } catch {
      return null;
    }
  }

  relay() {
    if (this.link) this.conn.connectRelay(this.link);
  }

  disconnect() {
    this.conn.disconnect();
    try {
      sessionStorage.removeItem(SESSION_KEY);
    } catch {
      // Nothing to forget.
    }
  }

  clientName() {
    return this.conn.state.client || "Your agent";
  }

  // --- State ------------------------------------------------------------------

  changed(s) {
    const b = this.button;
    b.dataset.state = s.status;
    const connected = s.status === "connected";
    this.label.textContent = connected ? `${s.client || "Agent"} connected` : s.status === "connecting" ? "Connecting…" : "Connect your AI agent";
    b.title = connected ? `Connected to ${this.clientName()}${s.via === "relay" ? " through the connection window" : ""}` : "Let an AI agent work on this model with you";
    document.documentElement.dataset.agent = s.status;

    const act = (label, run, primary = false) =>
      h("button", { type: "button", class: primary ? "primary" : "", onclick: run, "data-testid": `agent-${label.split(" ")[0].toLowerCase()}` }, label);
    const relay = (primary) => this.link && act("Open a connection window", () => this.relay(), primary);
    let body;
    switch (s.status) {
      case "connected":
        body = [
          h("p", {}, h("span", { class: "agent-live", "aria-hidden": "true" }), " Connected to ", h("strong", {}, this.clientName()), s.via === "relay" ? " through the connection window (keep it open)." : "."),
          h("p", { class: "agent-doing muted" }, (this.doingText = h("span", {}, "Ask it for something below, or anything else."))),
          act("Disconnect", () => this.disconnect()),
        ];
        break;
      case "connecting":
        body = s.via === "relay"
          ? [h("p", {}, "Connecting through the connection window…")]
          : [
              h("p", {}, "Connecting… If the browser asks whether this site may reach apps or devices on this computer, choose Allow."),
              relay(false),
            ];
        break;
      case "failed":
        if (s.reason === "direct") {
          body = [
            h("p", {}, "This browser did not let the page reach neoscad directly (Safari never does; Chrome and Edge do not once access to this computer's apps was refused). A small connection window works everywhere:"),
            relay(true),
            h("p", { class: "muted" }, "If that fails too, is your agent still running? Ask it for a fresh link."),
          ];
        } else if (s.reason === "popup-blocked") {
          body = [h("p", {}, "The browser blocked the connection window: allow pop-ups for this site, then try again."), relay(true)];
        } else {
          body = [h("p", {}, "Could not reach neoscad on this computer. Is your agent still running? Ask it for a fresh link, and paste it below.")];
        }
        break;
      case "closed":
        body = [
          h("p", {}, s.reason === "replaced" ? "Another tab took over the agent connection." : `Disconnected: ${s.reason}.`),
          this.link && act("Reconnect", () => this.connect(this.link)),
        ];
        break;
      default:
        body = [h("p", {}, "Not connected.")];
    }
    clear(this.status, body);
    this.status.dataset.state = s.status;
    // A link that failed while the user was not looking at the dialog
    // needs their click (the connection window is a popup).
    if (s.status === "failed" && !this.quiet) this.open();
    if (s.status !== "connecting") this.quiet = false;
  }

  /// What the agent is doing (a request in flight), or null when done.
  activity(method) {
    clearTimeout(this.idleTimer);
    // The agent acting on the page is what the user wants to watch (and an
    // edit may need their Apply, which the modal dialog would cover).
    if (SHOWN.has(method) && this.dialog.open) this.dialog.close();
    if (method) {
      this.button.classList.add("busy");
      const text = `${this.clientName()} ${DOING[method] ?? "is working"}…`;
      this.button.title = text;
      if (this.doingText) this.doingText.textContent = text;
    } else {
      this.idleTimer = setTimeout(() => {
        this.button.classList.remove("busy");
        this.changed(this.conn.state);
      }, 700);
    }
  }

  /// Ask the user to apply an edit: resolves true for Apply.
  confirm(message) {
    return new Promise((resolve) => {
      const done = (ok) => {
        clearTimeout(timer);
        this.confirmBar.hidden = true;
        resolve(ok);
      };
      const timer = setTimeout(() => done(false), CONFIRM_MS);
      clear(
        this.confirmBar,
        h("span", {}, message),
        h("button", { type: "button", class: "primary", onclick: () => done(true), "data-testid": "agent-apply" }, "Apply"),
        h("button", { type: "button", onclick: () => done(false), "data-testid": "agent-reject" }, "Reject"),
      );
      this.confirmBar.hidden = false;
      this.confirmBar.querySelector("button")?.focus();
    });
  }
}

async function copy(text, button) {
  try {
    await navigator.clipboard.writeText(text);
    const was = button.textContent;
    if (button.classList.contains("agent-copy")) {
      button.textContent = "Copied";
      setTimeout(() => (button.textContent = was), 1500);
    } else {
      button.classList.add("copied");
      setTimeout(() => button.classList.remove("copied"), 1500);
    }
  } catch {
    // No clipboard (an insecure context, or refused): the text is on screen.
  }
}
