// The check panel (apple/App/Panels/CheckPanel.swift): `neoscad check` on
// the document with a printer's numbers, and its findings listed with
// severity, code, message and fix. Selecting a finding marks it in the 3D
// view (a numbered marker at its worst point and its box) and turns the
// view to it. The printer settings are the page's, kept with the other
// settings, not the document's.

import { clear, fmt, h } from "./dom.js";

/// The macOS panel's presets (CheckPanel.swift's PrinterPreset.all).
export const PRINTERS = [
  { id: "prusa-mk4", name: "Prusa MK4", nozzle: 0.4, bed: [250, 210, 220] },
  { id: "prusa-mini", name: "Prusa MINI+", nozzle: 0.4, bed: [180, 180, 180] },
  { id: "bambu-x1", name: "Bambu Lab X1 / P1", nozzle: 0.4, bed: [256, 256, 256] },
  { id: "bambu-a1-mini", name: "Bambu Lab A1 mini", nozzle: 0.4, bed: [180, 180, 180] },
  { id: "ender-3", name: "Creality Ender-3", nozzle: 0.4, bed: [220, 220, 250] },
  { id: "voron-350", name: "Voron 2.4 (350)", nozzle: 0.4, bed: [350, 350, 340] },
];

/// `check`'s defaults (a 0.4 mm nozzle, 0.8 mm walls, 45°, no bed).
export const CHECK_DEFAULTS = { preset: "custom", nozzle: 0.4, minWall: 0.8, maxOverhang: 45, useBed: false, bed: [220, 220, 250] };

export function checkOptions(s) {
  const p = PRINTERS.find((x) => x.id === s.preset);
  return {
    nozzle: p ? p.nozzle : s.nozzle,
    minWall: s.minWall,
    maxOverhang: s.maxOverhang,
    bed: p ? p.bed : s.useBed ? s.bed : null,
  };
}

const SEVERITY_COLOR = { error: "#e5484d", warning: "#f5a524", info: "#3f8ff0" };

export class CheckPanel {
  /// `run()` runs the check with the current settings; `onSelect(finding
  /// | null)` marks it in the view; `onSettings(s)` keeps the settings;
  /// `onParts(on)` flips the document's parts toggle.
  constructor(root, { settings, run, onSelect, onSettings, onParts }) {
    this.root = root;
    this.settings = { ...CHECK_DEFAULTS, ...settings };
    this.runCheck = run;
    this.onSelect = onSelect;
    this.onSettings = onSettings;
    this.onParts = onParts;
    this.report = null;
    this.error = null;
    this.running = false;
    this.selected = null;
    this.parts = false;
    this.render();
  }

  setParts(on) {
    this.parts = on;
    this.render();
  }

  setRunning() {
    this.running = true;
    this.error = null;
    this.render();
  }

  setReport(report) {
    this.running = false;
    this.report = report;
    this.selected = null;
    this.onSelect(null);
    this.render();
  }

  setError(message) {
    this.running = false;
    this.error = message;
    this.render();
  }

  update(change) {
    this.settings = { ...this.settings, ...change };
    this.onSettings(this.settings);
    this.render();
  }

  select(f) {
    this.selected = this.selected === f.id ? null : f.id;
    this.onSelect(this.selected === null ? null : f);
    this.render();
  }

  render() {
    const s = this.settings;
    const custom = s.preset === "custom";
    const num = (key, label, unit, step = 0.1) =>
      h(
        "label",
        { class: "field" },
        label,
        h("input", {
          type: "number",
          step,
          min: 0,
          value: s[key],
          disabled: !custom && key === "nozzle",
          onchange: (e) => {
            const x = Number(e.target.value);
            if (Number.isFinite(x) && x > 0) this.update({ [key]: x });
            else e.target.value = s[key];
          },
        }),
        h("span", { class: "unit" }, unit),
      );
    const settings = h(
      "details",
      { class: "check-settings", open: true },
      h("summary", {}, "Printer"),
      h(
        "label",
        { class: "field" },
        "Preset",
        h(
          "select",
          { onchange: (e) => this.update({ preset: e.target.value }) },
          PRINTERS.map((p) => h("option", { value: p.id, selected: s.preset === p.id }, p.name)),
          h("option", { value: "custom", selected: custom }, "Custom"),
        ),
      ),
      num("nozzle", "Nozzle", "mm"),
      num("minWall", "Min wall", "mm"),
      num("maxOverhang", "Max overhang", "°", 1),
      custom
        ? h(
            "label",
            { class: "field" },
            h("input", { type: "checkbox", checked: s.useBed, onchange: (e) => this.update({ useBed: e.target.checked }) }),
            "Bed",
            s.bed.map((x, i) =>
              h("input", {
                type: "number",
                class: "narrow",
                value: x,
                disabled: !s.useBed,
                "aria-label": ["Width", "Depth", "Height"][i],
                onchange: (e) => {
                  const b = [...s.bed];
                  b[i] = Number(e.target.value) || b[i];
                  this.update({ bed: b });
                },
              }),
            ),
            h("span", { class: "unit" }, "mm"),
          )
        : null,
    );
    clear(
      this.root,
      h(
        "div",
        { class: "panel-toolbar" },
        h("button", { class: "primary", disabled: this.running, onclick: () => this.runCheck() }, this.running ? "Checking…" : "Check"),
        h(
          "label",
          { class: "toggle", title: "neoscad's part() extension: findings name parts" },
          h("input", { type: "checkbox", checked: this.parts, onchange: (e) => this.onParts(e.target.checked) }),
          "Parts",
        ),
      ),
      h("div", { class: "panel-scroll" }, settings, this.results()),
    );
  }

  results() {
    if (this.error) return h("p", { class: "panel-error" }, this.error);
    const r = this.report;
    if (!r) return h("p", { class: "panel-empty" }, "Check looks for walls too thin to print, overhangs, pieces off the bed and more.");
    if (r.failed) return h("p", { class: "panel-error" }, "The model did not render, so nothing was checked.");
    const summary = h(
      "p",
      { class: "check-summary", "data-testid": "check-summary" },
      `${r.errors} error${r.errors === 1 ? "" : "s"}, ${r.warnings} warning${r.warnings === 1 ? "" : "s"}, ${r.info} note${r.info === 1 ? "" : "s"}`,
      r.minWall != null ? ` · thinnest wall ${fmt(r.minWall, 2)} mm` : "",
    );
    if (!r.findings.length) return [summary, h("p", { class: "panel-empty" }, "No problems found.")];
    return [
      summary,
      h(
        "ol",
        { class: "findings" },
        r.findings.map((f) =>
          h(
            "li",
            {},
            h(
              "button",
              {
                class: `finding severity-${f.severity}${this.selected === f.id ? " selected" : ""}`,
                "aria-pressed": String(this.selected === f.id),
                onclick: () => this.select(f),
              },
              h(
                "span",
                { class: "finding-head" },
                h("span", { class: "finding-id" }, `${f.id}.`),
                h("span", { class: "badge", style: { background: SEVERITY_COLOR[f.severity] ?? "#888" } }, f.severity),
                h("code", {}, f.code),
                f.part ? h("span", { class: "muted" }, `in ${f.part}`) : null,
              ),
              h("span", { class: "finding-message" }, f.message),
              f.fix ? h("span", { class: "finding-fix" }, `Fix: ${f.fix}`) : null,
            ),
          ),
        ),
      ),
      r.truncated.length
        ? h("p", { class: "muted" }, r.truncated.map((t) => `${t.count} more ${t.code}`).join(", "))
        : null,
    ];
  }
}

/// A finding as the viewer's annotation.
export function findingMarker(f) {
  return {
    point: f.point,
    bboxMin: f.bboxMin ?? null,
    bboxMax: f.bboxMax ?? null,
    label: f.id,
    color: SEVERITY_COLOR[f.severity] ?? "#e0f",
  };
}
