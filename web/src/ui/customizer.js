// The customizer panel (apple/App/Panels/CustomizerView.swift): the
// document's parameters in their groups, each with OpenSCAD's control for
// it. Editing a value never touches the text; the page runs the document
// again with the values as overrides. The model (model/customizer.js)
// holds the values and their rules; this file only draws and forwards.
//
// Number fields commit on Enter or when they lose focus, so a half-typed
// "1." does not run the model; sliders commit as they move, since a run
// per step is what the macOS panel does too (runs are coalesced).

import { formatNumber, parseNumber } from "../model/customizer.js";
import { clear, h } from "./dom.js";

export class CustomizerPanel {
  /// `onChange()` is called after the model's values changed.
  constructor(root, model, { onChange }) {
    this.root = root;
    this.model = model;
    this.onChange = onChange;
    this.render();
  }

  setModel(model) {
    this.model = model;
    this.render();
  }

  changed() {
    this.render(true);
    this.onChange();
  }

  /// Draw the panel. A run's new parameters (`force` false) leave it alone
  /// while a field in it has focus: rebuilding would drop the text being
  /// typed. The field's blur draws it.
  render(force = false) {
    const focused = this.root.contains(document.activeElement) ? document.activeElement?.dataset?.param : null;
    if (focused && !force) return;
    const m = this.model;
    const toolbar = h(
      "div",
      { class: "panel-toolbar" },
      h(
        "button",
        {
          disabled: Object.keys(m.values).length === 0,
          title: "Return every value to the one in the text",
          onclick: () => m.reset() && this.changed(),
        },
        "Reset",
      ),
    );
    if (!m.groups.length) {
      clear(
        this.root,
        toolbar,
        h(
          "p",
          { class: "panel-empty" },
          "No parameters. Top-level assignments before the first module or function, with customizer comments, appear here.",
        ),
      );
      return;
    }
    clear(
      this.root,
      toolbar,
      h(
        "div",
        { class: "panel-scroll" },
        m.groups.map((g) =>
          h(
            "details",
            { class: "param-group", open: true },
            h("summary", {}, g.name),
            g.parameters.map((p) => this.row(p)),
          ),
        ),
      ),
    );
  }

  row(p) {
    const m = this.model;
    const set = m.isSet(p.name);
    return h(
      "div",
      { class: `param${set ? " is-set" : ""}`, "data-name": p.name },
      h(
        "div",
        { class: "param-head" },
        h("label", { class: "param-name", for: `param-${p.name}` }, p.name),
        set
          ? h(
              "button",
              {
                class: "icon-button",
                title: "Back to the text's value",
                "aria-label": `Reset ${p.name}`,
                onclick: () => m.unset(p.name) && this.changed(),
              },
              "↺",
            )
          : null,
      ),
      p.description ? h("div", { class: "param-description" }, p.description) : null,
      this.control(p),
    );
  }

  control(p) {
    const m = this.model;
    const c = p.control;
    const v = m.value(p);
    const commit = (x) => m.set(p, x) && this.changed();
    const id = `param-${p.name}`;
    switch (c.kind) {
      case "checkbox":
        return h("input", { id, type: "checkbox", checked: v === true, onchange: (e) => commit(e.target.checked) });
      case "slider": {
        const field = this.number(p, typeof v === "number" ? v : 0, (x) => commit(x), "narrow");
        // Dragging updates the value and runs without redrawing the panel,
        // which would replace the slider under the pointer and end the
        // drag; the release redraws it.
        return h(
          "div",
          { class: "param-row" },
          h("input", {
            id,
            type: "range",
            min: c.min,
            max: Math.max(c.min, c.max),
            step: c.step ?? "any",
            value: typeof v === "number" ? v : c.min,
            oninput: (e) => {
              if (!m.set(p, Number(e.target.value))) return;
              field.value = formatNumber(m.value(p));
              this.onChange();
            },
            onchange: () => this.render(true),
          }),
          field,
        );
      }
      case "spinbox":
        return h(
          "div",
          { class: "param-row" },
          this.number(p, typeof v === "number" ? v : 0, (x) => commit(x), null, id),
          h(
            "span",
            { class: "stepper" },
            h("button", { "aria-label": `Decrease ${p.name}`, onclick: () => commit((m.value(p) ?? 0) - (c.step ?? 1)) }, "−"),
            h("button", { "aria-label": `Increase ${p.name}`, onclick: () => commit((m.value(p) ?? 0) + (c.step ?? 1)) }, "+"),
          ),
        );
      case "text":
        return h("input", {
          id,
          type: "text",
          value: typeof v === "string" ? v : "",
          dataset: { param: p.name },
          onchange: (e) => commit(e.target.value),
          onkeydown: (e) => e.key === "Enter" && e.target.blur(),
        });
      case "vector": {
        const items = Array.isArray(v) ? v : [];
        return h(
          "div",
          { class: "param-row vector" },
          items.map((x, i) =>
            this.number(p, x, (y) => {
              const next = [...(Array.isArray(m.value(p)) ? m.value(p) : items)];
              next[i] = y;
              commit(next);
            }),
          ),
        );
      }
      case "dropdown": {
        const index = Math.max(
          0,
          c.options.findIndex((o) => JSON.stringify(o.value) === JSON.stringify(v)),
        );
        return h(
          "select",
          { id, onchange: (e) => commit(c.options[Number(e.target.value)].value) },
          c.options.map((o, i) => h("option", { value: String(i), selected: i === index }, o.label)),
        );
      }
      default:
        return null;
    }
  }

  /// A number field that commits on Enter or blur and puts back the value
  /// shown when the text is not a number.
  number(p, value, commit, cls = null, id = null) {
    const input = h("input", {
      id,
      type: "text",
      inputmode: "decimal",
      class: `number${cls ? ` ${cls}` : ""}`,
      value: formatNumber(value),
      dataset: { param: p.name },
      "aria-label": p.name,
    });
    const submit = () => {
      const x = parseNumber(input.value);
      if (x === null) input.value = formatNumber(value);
      else if (x !== value) commit(x);
    };
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") input.blur();
    });
    input.addEventListener("blur", () => {
      submit();
      // A redraw skipped while this field had focus happens now.
      queueMicrotask(() => this.render());
    });
    return input;
  }
}
