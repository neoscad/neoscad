// The customizer's logic, apart from the DOM: which values are set, what a
// control's edit turns into, and what the run is sent. Mirrors the macOS
// panel (apple/App/Panels/CustomizerView.swift): a value equal to the
// text's is not an override, a slider moves on its step grid from the
// minimum, and number fields clamp to the control's bounds.

export function valuesEqual(a, b) {
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((x, i) => x === b[i]);
  return a === b;
}

/// A slider's value on its step grid from `min`, keeping the step's
/// decimals (0.1 steps give 0.3, not 0.30000000000000004).
export function snap(x, step, min) {
  if (!step || step <= 0) return x;
  const n = Math.round((x - min) / step);
  const decimals = Math.max(0, -Math.floor(Math.log10(step)) + 1);
  const scale = 10 ** Math.min(decimals, 12);
  return Math.round((min + n * step) * scale) / scale;
}

export function clamp(x, min, max) {
  let v = x;
  if (min != null) v = Math.max(v, min);
  if (max != null) v = Math.min(v, max);
  return v;
}

/// A number as `%g` prints it, near enough for a field.
export function formatNumber(x) {
  if (!Number.isFinite(x)) return String(x);
  return String(Number(x.toPrecision(6)));
}

/// A number typed into a field: the number, or null for text that is not
/// one (a half-typed "1." still parses; "abc" does not).
export function parseNumber(s) {
  const t = String(s).trim();
  if (!t) return null;
  const x = Number(t);
  return Number.isFinite(x) ? x : null;
}

/// A string cut to at most `maxLength` UTF-8 bytes, on a character
/// boundary (as the macOS panel cuts it).
export function limitText(s, maxLength) {
  if (maxLength == null) return s;
  const enc = new TextEncoder();
  let out = "";
  let bytes = 0;
  for (const ch of s) {
    bytes += enc.encode(ch).length;
    if (bytes > maxLength) break;
    out += ch;
  }
  return out;
}

/// The customizer's state for one document: the groups the engine read
/// from its text, and the values the user set (by name, plain JSON).
export class CustomizerModel {
  constructor(groups = [], values = {}) {
    this.groups = groups;
    this.values = { ...values };
  }

  parameters() {
    const seen = new Map();
    for (const g of this.groups) for (const p of g.parameters) if (!seen.has(p.name)) seen.set(p.name, p);
    return seen;
  }

  /// New groups after a run: values for parameters that are gone, or that
  /// now equal the text's value, are dropped.
  setGroups(groups) {
    this.groups = groups;
    const params = this.parameters();
    for (const name of Object.keys(this.values)) {
      const p = params.get(name);
      if (!p || valuesEqual(p.defaultValue, this.values[name])) delete this.values[name];
    }
  }

  value(p) {
    return Object.hasOwn(this.values, p.name) ? this.values[p.name] : p.defaultValue;
  }

  isSet(name) {
    return Object.hasOwn(this.values, name);
  }

  /// Set a value from its control; the text's own value unsets it. Returns
  /// whether anything changed.
  set(p, v) {
    const before = this.values[p.name];
    const c = p.control;
    let value = v;
    if (c.kind === "slider" && typeof v === "number") value = clamp(snap(v, c.step, c.min), c.min, c.max);
    else if (c.kind === "spinbox" && typeof v === "number") value = clamp(v, c.min, c.max);
    else if (c.kind === "vector" && Array.isArray(v)) value = v.map((x) => clamp(x, c.min, c.max));
    else if (c.kind === "text" && typeof v === "string") value = limitText(v, c.maxLength);
    if (valuesEqual(value, p.defaultValue)) delete this.values[p.name];
    else this.values[p.name] = value;
    return !valuesEqual(before, this.values[p.name]);
  }

  unset(name) {
    const had = this.isSet(name);
    delete this.values[name];
    return had;
  }

  reset() {
    const had = Object.keys(this.values).length > 0;
    this.values = {};
    return had;
  }
}
