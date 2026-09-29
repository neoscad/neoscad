// A drop-down menu for the top bar (Export, View): a button and a list of
// items, rebuilt each time it opens so checkmarks show the current state.
// Items: {label, run, checked?, disabled?, shortcut?} or "-" for a divider.
// Closes on a choice, Escape, or a click outside.

import { clear, h } from "./dom.js";

export class Menu {
  constructor(label, items, { testid } = {}) {
    this.items = items;
    this.button = h(
      "button",
      {
        class: "menu-button",
        "aria-haspopup": "menu",
        "aria-expanded": "false",
        "data-testid": testid,
        onclick: (e) => {
          e.stopPropagation();
          this.toggle();
        },
      },
      label,
      h("span", { class: "caret", "aria-hidden": "true" }, "▾"),
    );
    this.list = h("div", { class: "menu", role: "menu", hidden: true });
    this.el = h("div", { class: "menu-wrap" }, this.button, this.list);
    document.addEventListener("click", (e) => {
      if (!this.el.contains(e.target)) this.close();
    });
    this.el.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        this.close();
        this.button.focus();
      }
    });
  }

  toggle() {
    if (this.list.hidden) this.open();
    else this.close();
  }

  open() {
    const items = typeof this.items === "function" ? this.items() : this.items;
    clear(
      this.list,
      items.map((it) =>
        it === "-"
          ? h("div", { class: "menu-divider", role: "separator" })
          : it.heading
            ? h("div", { class: "menu-heading" }, it.heading)
            : h(
                "button",
                {
                  class: "menu-item",
                  role: it.checked === undefined ? "menuitem" : "menuitemcheckbox",
                  "aria-checked": it.checked === undefined ? null : String(!!it.checked),
                  disabled: !!it.disabled,
                  onclick: () => {
                    this.close();
                    it.run();
                  },
                },
                h("span", { class: "check", "aria-hidden": "true" }, it.checked ? "✓" : ""),
                h("span", { class: "label" }, it.label),
                it.shortcut ? h("span", { class: "shortcut" }, it.shortcut) : null,
              ),
      ),
    );
    this.list.hidden = false;
    this.button.setAttribute("aria-expanded", "true");
    this.list.querySelector("button:not([disabled])")?.focus();
  }

  close() {
    this.list.hidden = true;
    this.button.setAttribute("aria-expanded", "false");
  }
}
