// The measure panel (apple/App/Panels/MeasurePanel.swift): the model's
// volume, area, box and centre, each part's, a section through the model
// or a part along an axis (drawn in the view), the distance between two
// parts, and picked points with the distance between them.
//
// Sections, distances and picks work from the worker's last measurement,
// so moving the slider cuts the same solid again without another render.

import { clear, fmt, h, vec } from "./dom.js";

export class MeasurePanel {
  /// Callbacks: `run()` measures; `section(axis, offset, target)`,
  /// `between(a, b)` ask the worker; `onParts(on)` flips parts;
  /// `onPicking(on)` turns picking in the view on or off.
  constructor(root, callbacks) {
    this.root = root;
    this.cb = callbacks;
    this.result = null;
    this.error = null;
    this.running = false;
    this.parts = false;
    this.section = { shown: false, axis: "z", offset: null, target: null, result: null };
    this.between = { a: null, b: null, result: null };
    this.picking = false;
    this.picks = [];
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

  setResult(r) {
    this.running = false;
    this.result = r;
    this.section.result = null;
    this.between.result = null;
    this.picks = [];
    const b = this.box();
    if (b) this.section.offset = (b.min[2] + b.max[2]) / 2;
    this.render();
  }

  setError(message) {
    this.running = false;
    this.error = message;
    this.render();
  }

  /// A section's numbers. Only their line is redrawn: redrawing the panel
  /// would replace the slider under the pointer and end the drag.
  setSection(r) {
    this.section.result = r;
    this.renderSectionResult();
  }

  renderSectionResult() {
    const s = this.section;
    const res = s.result;
    if (!this.sectionOut) return;
    this.sectionOut.textContent =
      s.shown && res
        ? res.bboxMin == null
          ? `The plane ${res.plane} misses the solid.`
          : `${res.plane}: area ${fmt(res.area, 3)} mm², perimeter ${fmt(res.perimeter, 3)} mm, ${res.contours} contour${res.contours === 1 ? "" : "s"}`
        : "";
  }

  setBetween(r) {
    this.between.result = r;
    this.render();
  }

  addPick(point) {
    this.picks = [...this.picks, point].slice(-2);
    this.render();
  }

  /// The box of what the section cuts (the model, or the chosen part).
  box() {
    const r = this.result;
    if (!r) return null;
    const solid = this.section.target ? r.parts?.find((p) => p.name === this.section.target)?.solid : r.model;
    return solid ? { min: solid.bboxMin, max: solid.bboxMax } : null;
  }

  updateSection() {
    const s = this.section;
    if (!s.shown || s.offset == null) {
      s.result = null;
      this.cb.section(null);
      this.render();
      return;
    }
    this.cb.section(s.axis, s.offset, s.target);
  }

  render() {
    clear(
      this.root,
      h(
        "div",
        { class: "panel-toolbar" },
        h("button", { class: "primary", disabled: this.running, onclick: () => this.cb.run() }, this.running ? "Measuring…" : "Measure"),
        h(
          "label",
          { class: "toggle" },
          h("input", { type: "checkbox", checked: this.parts, onchange: (e) => this.cb.onParts(e.target.checked) }),
          "Parts",
        ),
      ),
      h("div", { class: "panel-scroll" }, this.body()),
    );
  }

  stats(s) {
    return h(
      "dl",
      { class: "stats" },
      h("dt", {}, "Volume"),
      h("dd", {}, `${fmt(s.volume, 3)} mm³`),
      h("dt", {}, "Area"),
      h("dd", {}, `${fmt(s.area, 3)} mm²`),
      h("dt", {}, "Size"),
      h("dd", {}, s.bboxMax.map((x, i) => fmt(x - s.bboxMin[i], 3)).join(" × ")),
      h("dt", {}, "Box"),
      h("dd", {}, `${vec(s.bboxMin)} – ${vec(s.bboxMax)}`),
      h("dt", {}, "Centre"),
      h("dd", {}, vec(s.centroid)),
    );
  }

  body() {
    if (this.error) return h("p", { class: "panel-error" }, this.error);
    const r = this.result;
    if (!r) {
      return h(
        "p",
        { class: "panel-empty" },
        "Measure renders the model and reports its volume, area, box and centre, with sections and distances.",
      );
    }
    if (r.exitCode) return h("p", { class: "panel-error" }, "The model did not render.");
    if (!r.model && !r.model2d) return h("p", { class: "panel-empty" }, "The model is empty.");
    const out = [];
    if (r.model) {
      out.push(
        h("h3", {}, "Model"),
        this.stats(r.model),
        h(
          "p",
          { class: "muted" },
          `${r.components ?? "?"} component${r.components === 1 ? "" : "s"}, ${r.manifold ? "manifold" : "not manifold"}`,
        ),
      );
    } else if (r.model2d) {
      out.push(h("h3", {}, "Model (2D)"), h("p", {}, `Area ${fmt(r.model2d.area, 3)} mm²`));
    }
    if (r.parts?.length) {
      out.push(
        h("h3", {}, "Parts"),
        r.parts.map((p) =>
          h(
            "details",
            { class: "part" },
            h("summary", {}, p.name, p.instances > 1 ? ` ×${p.instances}` : "", p.context ? h("span", { class: "muted" }, ` (in ${p.context})`) : null),
            p.solid ? this.stats(p.solid) : h("p", { class: "muted" }, "Not a solid"),
          ),
        ),
      );
      out.push(this.betweenView(r));
    }
    if (r.model) out.push(this.sectionView(r));
    out.push(this.pickView());
    return out;
  }

  betweenView(r) {
    const names = r.parts.map((p) => p.name);
    const picker = (label, key) =>
      h(
        "label",
        { class: "field" },
        label,
        h(
          "select",
          {
            onchange: (e) => {
              this.between[key] = e.target.value || null;
              this.between.result = null;
              if (this.between.a && this.between.b) this.cb.between(this.between.a, this.between.b);
              else this.render();
            },
          },
          h("option", { value: "" }, "None"),
          names.map((n) => h("option", { value: n, selected: this.between[key] === n }, n)),
        ),
      );
    const b = this.between.result;
    return h(
      "section",
      { class: "measure-between" },
      h("h3", {}, "Between parts"),
      picker("From", "a"),
      picker("To", "b"),
      b
        ? h(
            "p",
            { "data-testid": "between-result" },
            b.overlapping
              ? `Overlapping (${fmt(b.overlapVolume, 3)} mm³)`
              : b.distance == null
                ? "Not solids."
                : `Distance ${fmt(b.distance, 3)} mm${b.touching ? " (touching)" : ""}`,
          )
        : null,
    );
  }

  sectionView(r) {
    const s = this.section;
    const b = this.box();
    const k = { x: 0, y: 1, z: 2 }[s.axis];
    const lo = b ? b.min[k] : 0;
    const hi = b ? b.max[k] : 0;
    this.sectionOut = h("p", { "data-testid": "section-result" });
    this.renderSectionResult();
    return h(
      "section",
      { class: "measure-section" },
      h(
        "h3",
        {},
        h(
          "label",
          { class: "toggle" },
          h("input", {
            type: "checkbox",
            checked: s.shown,
            onchange: (e) => {
              s.shown = e.target.checked;
              if (s.offset == null || s.offset < lo || s.offset > hi) s.offset = (lo + hi) / 2;
              this.updateSection();
              this.render();
            },
          }),
          "Section",
        ),
      ),
      r.parts?.length
        ? h(
            "label",
            { class: "field" },
            "Cut",
            h(
              "select",
              {
                onchange: (e) => {
                  s.target = e.target.value || null;
                  this.updateSection();
                  this.render();
                },
              },
              h("option", { value: "" }, "Model"),
              r.parts.filter((p) => p.solid).map((p) => h("option", { value: p.name, selected: s.target === p.name }, p.name)),
            ),
          )
        : null,
      h(
        "label",
        { class: "field" },
        "Axis",
        h(
          "select",
          {
            onchange: (e) => {
              s.axis = e.target.value;
              const kk = { x: 0, y: 1, z: 2 }[s.axis];
              if (b) s.offset = (b.min[kk] + b.max[kk]) / 2;
              this.updateSection();
              this.render();
            },
          },
          ["x", "y", "z"].map((a) => h("option", { value: a, selected: s.axis === a }, a.toUpperCase())),
        ),
      ),
      h(
        "label",
        { class: "field" },
        "At",
        h("input", {
          type: "range",
          min: lo,
          max: hi,
          step: (hi - lo) / 200 || 0.01,
          value: s.offset ?? (lo + hi) / 2,
          disabled: !s.shown,
          oninput: (e) => {
            s.offset = Number(e.target.value);
            e.target.nextSibling.textContent = `${fmt(s.offset, 2)} mm`;
            this.updateSection();
          },
        }),
        h("span", { class: "unit" }, `${fmt(s.offset, 2)} mm`),
      ),
      this.sectionOut,
    );
  }

  pickView() {
    const [a, b] = this.picks;
    return h(
      "section",
      { class: "measure-pick" },
      h(
        "h3",
        {},
        h(
          "label",
          { class: "toggle" },
          h("input", {
            type: "checkbox",
            checked: this.picking,
            onchange: (e) => {
              this.picking = e.target.checked;
              this.picks = [];
              this.cb.onPicking(this.picking);
              this.render();
            },
          }),
          "Pick points",
        ),
      ),
      this.picking && !a ? h("p", { class: "muted" }, "Click the model to pick a point.") : null,
      a ? h("p", {}, `A: ${vec(a)}`) : null,
      b ? h("p", {}, `B: ${vec(b)}`) : null,
      a && b ? h("p", {}, `Distance: ${fmt(Math.hypot(...a.map((x, i) => x - b[i])), 3)} mm`) : null,
      this.picks.length
        ? h(
            "button",
            {
              onclick: () => {
                this.picks = [];
                this.cb.onPicking(this.picking);
                this.render();
              },
            },
            "Clear",
          )
        : null,
    );
  }
}
