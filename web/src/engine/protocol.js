// The worker protocol as the front end assumes it, in one place.
//
// The engine runs in a module worker (crates/web, docs/web-protocol.md once
// it lands). Until then this file is the front end's side of a draft built
// from the message list in docs/web-demo-plan.md. Everything the rest of
// the front end knows about the wire goes through here: the envelope,
// the file layout, and the normalisers that turn the worker's JSON into
// the shapes the panels use. Aligning with the final protocol should be a
// change to this file (and the mock that speaks it), not to the panels.
//
// Envelope (draft):
//   main -> worker  {id, type, ...fields}          a request, answered once
//                   {type: "lsp", message}          no id: a notification
//   worker -> main  {id, ok: true, result}          the answer
//                   {id, ok: false, error: {message, code?}}
//                   {type: "lsp", message}          a language-server message
//                   {type: "fatal", message}        the instance is dead
//                                                   (panic, OOM): respawn
//
// Requests: init, open, edit, run, parameters, check, measure, section,
// between, pick, export, addFiles, read, cancel. See `Requests` below
// for their fields. Result keys may be snake_case (serde's default) or
// camelCase; enums may be serde's externally tagged form or an internal
// `kind`/`type` tag. The normalisers accept all of those, so the draft's
// guesses about naming cannot break the panels.

/// Where documents and libraries live in the worker's in-memory file
/// system. MCAD is at `/neoscad/libraries` in crates/wasm-check already;
/// the working directory for the examples is a guess.
export const DOC_ROOT = "/neoscad/work";
export const LIB_ROOT = "/neoscad/libraries";

export const docPath = (file) => `${DOC_ROOT}/${file}`;
export const fileURI = (path) => `file://${path.split("/").map(encodeURIComponent).join("/")}`;
export const uriPath = (uri) =>
  uri.startsWith("file://") ? uri.slice(7).split("/").map(decodeURIComponent).join("/") : uri;

/// Builders for each request, so that the fields sent are written once.
export const Requests = {
  init: (options = {}) => ({ type: "init", options }),
  open: (path, text) => ({ type: "open", path, uri: fileURI(path), text }),
  /// `edits` are `[from, to, insert]` in UTF-16 offsets, each applied to
  /// the text the previous one left (web/src/editor-host.js gets them
  /// from the editor's bridge in that form). The full text rides along
  /// so a worker that prefers replacing the text can ignore the edits.
  edit: (path, version, edits, text) => ({ type: "edit", path, version, edits, text }),
  run: (path, mode, overrides, parts, enable = []) => ({
    type: "run",
    path,
    mode,
    overrides,
    parts,
    enable,
  }),
  parameters: (path) => ({ type: "parameters", path }),
  /// `options` as the check panel keeps them ({nozzle, minWall,
  /// maxOverhang, bed: [w, d, h] | null}), sent in the ffi's
  /// `CheckOptions` names; omitted fields take the core's defaults.
  check: (path, overrides, parts, options = {}) => ({
    type: "check",
    path,
    overrides,
    parts,
    options: {
      nozzle: options.nozzle,
      min_wall: options.minWall,
      max_overhang: options.maxOverhang,
      bed: options.bed ?? null,
    },
  }),
  measure: (path, overrides, parts) => ({ type: "measure", path, overrides, parts }),
  /// A section of the last measurement's model (`target` null) or part.
  section: (axis, offset, target = null) => ({ type: "section", axis, offset, target }),
  between: (a, b) => ({ type: "between", a, b }),
  /// A ray through the view, from the viewer's `ray_at`, against the last
  /// measurement's solids.
  pick: (origin, direction) => ({ type: "pick", origin, direction }),
  export: (path, format, overrides, parts) => ({ type: "export", path, format, overrides, parts }),
  /// `files` are `{path, bytes}` (an ArrayBuffer) or `{path, text}`.
  addFiles: (files) => ({ type: "addFiles", files }),
  read: (path) => ({ type: "read", path }),
  cancel: () => ({ type: "cancel" }),
};

export const EXPORT_FORMATS = {
  stl: { label: "STL", mime: "model/stl", ext: "stl" },
  "3mf": { label: "3MF", mime: "model/3mf", ext: "3mf" },
  off: { label: "OFF", mime: "text/plain", ext: "off" },
  svg: { label: "SVG (2D)", mime: "image/svg+xml", ext: "svg" },
};

// --- Normalisers -----------------------------------------------------------

const camel = (k) => k.replace(/_([a-z0-9])/g, (_, c) => c.toUpperCase());

/// Keys to camelCase, deeply; typed arrays and buffers pass through.
export function camelize(v) {
  if (Array.isArray(v)) return v.map(camelize);
  if (v === null || typeof v !== "object") return v;
  if (ArrayBuffer.isView(v) || v instanceof ArrayBuffer) return v;
  const out = {};
  for (const [k, x] of Object.entries(v)) out[camel(k)] = camelize(x);
  return out;
}

/// An enum as `{kind, ...fields}` with a lower-case kind, from any of
/// "Name", {"Name": {...}}, {kind: "name", ...} or {type: "name", ...}.
export function tagged(v) {
  if (typeof v === "string") return { kind: v.toLowerCase() };
  if (v === null || typeof v !== "object") return { kind: String(v) };
  if (typeof v.kind === "string") return { ...v, kind: v.kind.toLowerCase().replace(/_/g, "") };
  if (typeof v.type === "string") {
    const { type, ...rest } = v;
    return { ...rest, kind: type.toLowerCase().replace(/_/g, "") };
  }
  const keys = Object.keys(v);
  if (keys.length === 1 && /^[A-Z]/.test(keys[0])) {
    const inner = v[keys[0]];
    return { ...(inner && typeof inner === "object" ? inner : {}), kind: keys[0].toLowerCase() };
  }
  return { ...v, kind: "unknown" };
}

/// A customizer value as plain JSON (a boolean, number, string or array of
/// numbers), from plain JSON or the ffi's tagged `ParameterValue`.
export function plainValue(v) {
  if (v === null || typeof v !== "object" || Array.isArray(v)) return v;
  const t = tagged(v);
  return t.value;
}

/// A parameter's control: {kind: checkbox | slider | spinbox | text |
/// vector | dropdown, ...bounds}. Dropdown options carry plain values.
export function control(c) {
  const t = camelize(tagged(c));
  const num = (x) => (typeof x === "number" && Number.isFinite(x) ? x : null);
  switch (t.kind) {
    case "checkbox":
      return { kind: "checkbox" };
    case "slider":
      return { kind: "slider", min: num(t.min) ?? 0, max: num(t.max) ?? 0, step: num(t.step) };
    case "spinbox":
      return { kind: "spinbox", min: num(t.min), max: num(t.max), step: num(t.step) };
    case "text":
      return { kind: "text", maxLength: num(t.maxLength) };
    case "vector":
      return { kind: "vector", min: num(t.min), max: num(t.max), step: num(t.step) };
    case "dropdown":
      return {
        kind: "dropdown",
        options: (t.options ?? []).map((o) => ({ label: String(o.label), value: plainValue(o.value) })),
      };
    default:
      return { kind: "text", maxLength: null };
  }
}

/// Customizer groups: [{name, parameters: [{name, description, control,
/// defaultValue}]}].
export function parameterGroups(groups) {
  return (groups ?? []).map((g) => ({
    name: String(g.name ?? ""),
    parameters: (g.parameters ?? []).map((p) => ({
      name: String(p.name),
      description: String(p.description ?? ""),
      control: control(p.control),
      defaultValue: plainValue(p.defaultValue ?? p.default_value ?? p.default),
    })),
  }));
}

const CONSOLE_KINDS = new Set(["error", "warning", "deprecated", "echo", "trace", "info"]);

/// Console lines: [{kind, text, location: {path, startLine,
/// startCharacter, endLine, endCharacter} | null}].
export function consoleLines(lines) {
  return (lines ?? []).map((l) => {
    const c = camelize(l);
    const kind = tagged(c.kind ?? "info").kind;
    return {
      kind: CONSOLE_KINDS.has(kind) ? kind : "info",
      text: String(c.text ?? ""),
      location: c.location ? { ...c.location, path: uriPath(String(c.location.path ?? "")) } : null,
    };
  });
}

/// A run's result: {exitCode, geometry, timings, console, scene,
/// language, parameters?, fileView?}. `scene` is the packed scene for the
/// viewer (render::packed), passed through untouched.
export function runResult(r) {
  const scene = r?.scene ?? null;
  const c = camelize({ ...r, scene: null });
  const render = c.render ?? c;
  return {
    exitCode: render.exitCode ?? 0,
    geometry: render.geometry ?? null,
    timings: render.timings ?? null,
    console: consoleLines(c.console),
    language: c.language ?? [],
    parameters: c.parameters ? parameterGroups(c.parameters) : null,
    fileView: c.fileView ?? null,
    scene,
  };
}

/// A check report: camelCase, with findings' severities as lower-case strings.
export function checkReport(r) {
  const c = camelize(r ?? {});
  return {
    ...c,
    findings: (c.findings ?? []).map((f) => ({ ...f, severity: tagged(f.severity).kind })),
    truncated: c.truncated ?? [],
    parts: c.parts ?? [],
  };
}

export const measureResult = (r) => camelize(r ?? {});
export const sectionResult = (r) => camelize(r ?? {});
export const betweenResult = (r) => camelize(r ?? {});

// --- Helpers the host and the tests share ------------------------------------

/// The editor's edits (`[from, to, insert]`, UTF-16, sequential) applied
/// to a copy of the text, as the macOS app keeps its copy.
export function applyEdits(text, edits) {
  for (const [from, to, insert] of edits) {
    text = text.slice(0, from) + insert + text.slice(to);
  }
  return text;
}

/// Whether a text includes or uses BOSL2, so its files must be in the
/// worker before the run (they are fetched on first use; plan: "lazy
/// BOSL2"). Comments are not stripped: a commented-out include costs one
/// needless fetch, which is cheaper than a parser here.
export function usesLibrary(text, name) {
  const re = new RegExp(`\\b(?:include|use)\\s*<\\s*${name}/`);
  return re.test(text);
}

/// Customizer values as the run's overrides: `[{name, value}]` for the
/// values set, in name order so the request is deterministic.
export function overrides(values) {
  return Object.keys(values)
    .sort()
    .map((name) => ({ name, value: values[name] }));
}
