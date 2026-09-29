// The console under the editor, as the macOS app's
// (apple/App/Panels/ConsoleView.swift): the last run's summary, then every
// line in order with its kind's colour, filters by kind with counts, a
// text filter, a collapse toggle, and click-to-jump for lines that point
// into a file.

import { clear, fmt, h } from "./dom.js";

export const FILTERS = [
  { id: "errors", title: "Errors", kinds: ["error", "trace"], icon: "✖" },
  { id: "warnings", title: "Warnings", kinds: ["warning", "deprecated"], icon: "▲" },
  { id: "echo", title: "Echo", kinds: ["echo"], icon: "›" },
  { id: "other", title: "Other", kinds: ["info"], icon: "i" },
];

/// The one-line summary of a run (ConsoleView.describe).
export function describe(r, mode) {
  const ms = r.timings ? `${fmt(r.timings.totalMs ?? 0, 1)} ms` : "";
  if (mode === "preview") return r.exitCode === 0 ? `Previewed in ${ms}.` : `Preview failed (${ms}).`;
  if (r.exitCode !== 0) return `Render failed (${ms}).`;
  const g = r.geometry;
  if (!g) return `Rendered in ${ms}: empty result.`;
  const size = g.bboxMax.map((x, i) => fmt(x - g.bboxMin[i])).join(" × ");
  const parts = [`${g.dimensions}D`, `bbox ${size}`];
  if (g.volume != null) parts.push(`volume ${fmt(g.volume)}`);
  parts.push(`area ${fmt(g.area)}`);
  if (g.triangles != null) parts.push(`${g.triangles} triangles`);
  if (g.components != null) parts.push(`${g.components} component${g.components === 1 ? "" : "s"}`);
  if (g.manifold != null) parts.push(g.manifold ? "manifold" : "not manifold");
  return `Rendered in ${ms}: ${parts.join(", ")}`;
}

export class ConsolePanel {
  constructor(root, { onJump }) {
    this.root = root;
    this.onJump = onJump;
    this.lines = [];
    this.shown = new Set(FILTERS.map((f) => f.id));
    this.search = "";
    this.collapsed = false;
    this.summaryText = "Preview (F5) previews the model; Render (F6) renders it.";
    this.summaryKind = "idle";
    this.summary = h("span", { class: "console-summary", "data-testid": "render-summary" });
    this.toggles = h("span", { class: "console-filters" });
    this.list = h("div", { class: "console-lines", role: "log", "aria-live": "polite" });
    this.collapse = h("button", {
      class: "icon-button",
      onclick: () => {
        this.collapsed = !this.collapsed;
        this.render();
      },
    });
    this.filter = h("input", {
      type: "search",
      placeholder: "Filter",
      class: "console-search",
      "aria-label": "Filter console lines",
      oninput: (e) => {
        this.search = e.target.value.trim().toLowerCase();
        this.renderLines();
      },
    });
    clear(root, h("div", { class: "console-header" }, this.collapse, this.summary, this.toggles, this.filter), this.list);
    this.render();
  }

  setSummary(text, kind = "done") {
    this.summaryText = text;
    this.summaryKind = kind;
    this.renderSummary();
  }

  setLines(lines) {
    this.lines = lines;
    this.render();
  }

  render() {
    this.root.classList.toggle("collapsed", this.collapsed);
    this.collapse.textContent = this.collapsed ? "▸" : "▾";
    this.collapse.setAttribute("aria-label", this.collapsed ? "Show console" : "Hide console");
    this.renderSummary();
    clear(
      this.toggles,
      FILTERS.map((f) => {
        const n = this.lines.filter((l) => f.kinds.includes(l.kind)).length;
        return h(
          "button",
          {
            class: `filter-toggle kind-${f.id}`,
            "aria-pressed": String(this.shown.has(f.id)),
            title: `Show ${f.title.toLowerCase()} lines`,
            "aria-label": `${f.title} ${n}`,
            onclick: () => {
              if (this.shown.has(f.id)) this.shown.delete(f.id);
              else this.shown.add(f.id);
              this.render();
            },
          },
          h("span", { class: "filter-icon", "aria-hidden": "true" }, f.icon),
          ` ${n}`,
        );
      }),
    );
    this.renderLines();
  }

  renderSummary() {
    this.summary.className = `console-summary summary-${this.summaryKind}`;
    clear(this.summary, this.summaryKind === "running" ? h("span", { class: "spinner", "aria-hidden": "true" }) : null, this.summaryText);
    this.summary.title = this.summaryTitle ?? "";
  }

  renderLines() {
    const kinds = new Set(FILTERS.filter((f) => this.shown.has(f.id)).flatMap((f) => f.kinds));
    const rows = this.lines
      .filter((l) => kinds.has(l.kind) && (!this.search || l.text.toLowerCase().includes(this.search)))
      .map((l) => {
        const row = h(
          l.location ? "button" : "div",
          {
            class: `console-line kind-${l.kind}${l.location ? " jumps" : ""}`,
            title: l.location ? "Show in the editor" : null,
            onclick: l.location ? () => this.onJump(l.location) : null,
          },
          h("span", { class: "line-icon", "aria-hidden": "true" }, ICONS[l.kind] ?? ""),
          h("span", { class: "line-text" }, l.text),
          l.location ? h("span", { class: "line-jump", "aria-hidden": "true" }, "→") : null,
        );
        return row;
      });
    clear(this.list, rows);
  }
}

const ICONS = { error: "✖", trace: "↳", warning: "▲", deprecated: "◷", echo: "›", info: "i" };
