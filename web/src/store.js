// What the page keeps in localStorage: each example's edited text, its
// customizer values, and the settings (view toggles, the last example,
// the inspector's tab). Keys are versioned so a later layout can ignore
// old entries rather than misread them.
//
// Storage can be missing (a private window may throw on access) or full;
// then the page works as before and simply forgets on reload, rather than
// failing on a write.

const PREFIX = "neoscad.try.v1.";

export class Store {
  constructor(storage) {
    this.storage = storage ?? null;
  }

  static fromWindow(win = globalThis) {
    try {
      const s = win.localStorage;
      s.getItem("x");
      return new Store(s);
    } catch {
      return new Store(null);
    }
  }

  get(key, fallback = null) {
    try {
      const v = this.storage?.getItem(PREFIX + key);
      return v == null ? fallback : JSON.parse(v);
    } catch {
      return fallback;
    }
  }

  set(key, value) {
    try {
      if (value === undefined || value === null) this.storage?.removeItem(PREFIX + key);
      else this.storage?.setItem(PREFIX + key, JSON.stringify(value));
      return true;
    } catch {
      return false;
    }
  }

  // --- Examples ---

  /// The edited text of an example, or null when it is as shipped.
  exampleText(id) {
    return this.get(`example.${id}.text`);
  }

  /// Keep an example's text; the shipped text itself is not stored, so an
  /// example edited back to what it was is "unedited" again.
  setExampleText(id, text, original) {
    return this.set(`example.${id}.text`, text === original ? null : text);
  }

  /// Forget an example's edits and customizer values ("Reset example").
  resetExample(id) {
    this.set(`example.${id}.text`, null);
    this.set(`example.${id}.params`, null);
  }

  parameterValues(id) {
    const v = this.get(`example.${id}.params`, {});
    return v && typeof v === "object" && !Array.isArray(v) ? v : {};
  }

  setParameterValues(id, values) {
    return this.set(`example.${id}.params`, Object.keys(values).length ? values : null);
  }

  // --- Settings ---

  settings(defaults) {
    const s = this.get("settings", {});
    return { ...defaults, ...(s && typeof s === "object" ? s : {}) };
  }

  setSettings(settings) {
    return this.set("settings", settings);
  }
}

/// A Storage stand-in over a Map, for the tests (and nothing else).
export class MemoryStorage {
  constructor() {
    this.map = new Map();
  }
  getItem(k) {
    return this.map.has(k) ? this.map.get(k) : null;
  }
  setItem(k, v) {
    this.map.set(k, String(v));
  }
  removeItem(k) {
    this.map.delete(k);
  }
  get length() {
    return this.map.size;
  }
}
