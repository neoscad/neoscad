// The worker protocol (docs/web-protocol.md) as the front end speaks it, in
// one place. The engine is crates/web in a module worker (its reference
// glue, crates/web/js/worker.js, is the bundle's core/worker.js); the mock
// (mock-core.js) answers the same messages with canned results.
//
// Everything the rest of the page knows about the wire goes through here:
// the envelope's error kinds, where files live in the worker's memory, the
// request builders, and the few conversions between the page's shapes and
// the wire's (editor edits, customizer values, annotations' geometry).
//
// Envelope:
//   page -> worker  {id, type, ...fields}            one reply each, in order
//   worker -> page  {id, ok: true, result}
//                   {id, ok: false, error: {kind, message}}
//                   {type: "ready", version}          no id: the module loaded
//                   {type: "crashed", message}        no id: the instance trapped
//
// There is no cancel request: a run cannot be interrupted from inside the
// worker's one thread, so cancelling is terminating the worker and
// starting another (client.js).

/// Where documents and libraries live in the worker's in-memory file
/// system (docs/web-protocol.md, "Paths and documents").
export const DOC_ROOT = "/doc";
export const LIB_ROOT = "/neoscad/libraries";
/// Where the bundled fonts go once fetched (fonts.tar.gz; crates/web's
/// FONT_DIR): the core does not compile them in.
export const FONT_ROOT = "/neoscad/fonts";

export const docPath = (file) => `${DOC_ROOT}/${file}`;
export const fileURI = (path) => `file://${path.split("/").map(encodeURIComponent).join("/")}`;
export const uriPath = (uri) =>
  uri.startsWith("file://") ? uri.slice(7).split("/").map(decodeURIComponent).join("/") : uri;

/// `error.kind`s. `cancelled` means a newer request stopped this one (drop
/// the result); `crashed` and `panicked` mean the instance is gone
/// (respawn); `invalidArgument` is a bug in the page; `failed` is for the
/// user (a missing file, a bad archive).
export const ErrorKind = {
  cancelled: "cancelled",
  invalidArgument: "invalidArgument",
  failed: "failed",
  panicked: "panicked",
  crashed: "crashed",
};

/// A customizer value (plain JSON: a boolean, number, string or array of
/// numbers) as the wire's tagged `ParameterValue`.
export function parameterValue(v) {
  if (typeof v === "boolean") return { kind: "bool", value: v };
  if (typeof v === "number") return { kind: "number", value: v };
  if (Array.isArray(v)) return { kind: "vector", value: v.map(Number) };
  return { kind: "text", value: String(v) };
}

/// Customizer values (by name, plain JSON) as `[ParameterOverride]`, in
/// name order so the request is deterministic.
export function overrides(values) {
  return Object.keys(values)
    .sort()
    .map((name) => ({ name, value: parameterValue(values[name]) }));
}

/// `RunOptions` for detached requests (check, measure, export).
export const runOptions = (values, parts = false, enable = []) => ({ overrides: overrides(values), parts, enable });

/// Builders for each request, so that the fields sent are written once.
export const Requests = {
  /// `seed` is the seed of unseeded `rands()`: the page keeps one across
  /// respawns, so a respawn does not change a model. Limits default to the
  /// worker's (Limits::AGENT with 1 GiB of memory).
  init: (seed) => ({ type: "init", seed }),
  defaults: () => ({ type: "defaults" }),
  stats: () => ({ type: "stats" }),
  open: (path, text) => ({ type: "open", path, text }),
  update: (path, text) => ({ type: "update", path, text }),
  /// `edits` are `{start, end, text}` in LSP positions (editorEdits).
  edit: (path, edits) => ({ type: "edit", path, edits }),
  close: (path) => ({ type: "close", path }),
  /// `camera` ({vpt, vpr, vpd, vpf}, the view the model is shown in) sets
  /// `$vp*`; `colorScheme` names the scheme the face colours are baked in.
  run: ({ path, mode, values = {}, parts = false, enable = [], camera = null, colorScheme = null }) => {
    const r = { type: "run", path, mode, overrides: overrides(values), parts, enable };
    if (camera?.vpt && camera?.vpr && camera.vpd != null) {
      r.camera = { vpt: camera.vpt, vpr: camera.vpr, vpd: camera.vpd, vpf: camera.vpf ?? 22.5 };
    }
    if (colorScheme) r.colorScheme = colorScheme;
    return r;
  },
  parameters: (path) => ({ type: "parameters", path }),
  /// `options` are a whole `CheckOptions`: the page merges the panel's
  /// settings over the worker's `defaults`, because the wire has no
  /// per-field defaults.
  check: (path, run, options) => ({ type: "check", path, run, options }),
  measure: (path, run) => ({ type: "measure", path, run }),
  /// Measurement requests name the handle `measure` returned; the worker
  /// keeps only the latest, and none survives a respawn.
  section: (measurement, axis, offset, part = null) => {
    const r = { type: "section", measurement, axis, offset };
    if (part) r.part = part;
    return r;
  },
  between: (measurement, a, b) => ({ type: "between", measurement, a, b }),
  pick: (measurement, origin, direction) => ({ type: "pick", measurement, origin, direction }),
  export: (path, format, run, creationDate = new Date().toISOString().replace(/\.\d+Z$/, "Z")) => ({
    type: "export",
    path,
    format,
    run,
    creationDate,
  }),
  /// `files` are `{path, data}` (a string or an ArrayBuffer); `tar` an
  /// uncompressed ustar archive unpacked under `root`. Not transferred:
  /// the client keeps its copy to replay after a respawn.
  addFiles: ({ files = [], tar = null, root = null } = {}) => {
    const r = { type: "addFiles", files };
    if (tar) r.tar = tar;
    if (root) r.root = root;
    return r;
  },
  readFile: (path) => ({ type: "readFile", path }),
  lsp: (message) => ({ type: "lsp", message }),
};

/// The Export menu's formats. `extension` names the NeoSCAD extension a
/// format needs (`--enable` on the command line): the menu lists it only
/// while the Export menu's toggle for it is on, and the export sends it.
export const EXPORT_FORMATS = {
  stl: { label: "STL", ext: "stl" },
  "3mf": { label: "3MF", ext: "3mf" },
  off: { label: "OFF", ext: "off" },
  svg: { label: "SVG (2D)", ext: "svg" },
  step: { label: "STEP (exact surfaces)", ext: "step", extension: "exact" },
};

// --- Editor edits --------------------------------------------------------------

/// The LSP position (0-based line, UTF-16 column) of a UTF-16 offset.
export function positionAt(text, offset) {
  let line = 0;
  let start = 0;
  for (let i = text.indexOf("\n"); i >= 0 && i < offset; i = text.indexOf("\n", i + 1)) {
    line += 1;
    start = i + 1;
  }
  return { line, character: offset - start };
}

/// The UTF-16 offset of an LSP position, a column past the line's end
/// clamping to it (as the worker's `lang::source` does).
export function offsetAt(text, { line, character }) {
  let start = 0;
  for (let l = 0; l < line; l++) {
    const i = text.indexOf("\n", start);
    if (i < 0) return text.length;
    start = i + 1;
  }
  let end = text.indexOf("\n", start);
  if (end < 0) end = text.length;
  return Math.min(start + character, end);
}

/// The editor's edits (`[from, to, insert]`, UTF-16 offsets, each applied
/// to the text the previous one left; web/src/editor-host.js gets them
/// from the editor's bridge in that form) as the wire's `{start, end,
/// text}`, each in positions of the text it applies to, and the text after
/// them all.
export function editorEdits(text, edits) {
  const out = [];
  for (const [from, to, insert] of edits) {
    out.push({ start: positionAt(text, from), end: positionAt(text, to), text: insert });
    text = text.slice(0, from) + insert + text.slice(to);
  }
  return { edits: out, text };
}

/// The editor's edits applied to a copy of the text, as the macOS app
/// keeps its copy.
export const applyEdits = (text, edits) => editorEdits(text, edits).text;

// --- Results -------------------------------------------------------------------

/// A parameter's control as the customizer panel keys it: the wire's kinds
/// with `spinBox` lower-cased, numbers checked. An unknown kind becomes a
/// text field rather than breaking the panel.
export function control(c) {
  const num = (x) => (typeof x === "number" && Number.isFinite(x) ? x : null);
  switch (c?.kind) {
    case "checkbox":
      return { kind: "checkbox" };
    case "slider":
      return { kind: "slider", min: num(c.min) ?? 0, max: num(c.max) ?? 0, step: num(c.step) };
    case "spinBox":
      return { kind: "spinbox", min: num(c.min), max: num(c.max), step: num(c.step) };
    case "text":
      return { kind: "text", maxLength: num(c.maxLength) };
    case "vector":
      return { kind: "vector", min: num(c.min), max: num(c.max), step: num(c.step) };
    case "dropdown":
      return {
        kind: "dropdown",
        options: (c.options ?? []).map((o) => ({ label: String(o.label), value: plainValue(o.value) })),
      };
    default:
      return { kind: "text", maxLength: null };
  }
}

/// A wire `ParameterValue` as plain JSON.
export const plainValue = (v) => (v !== null && typeof v === "object" && !Array.isArray(v) ? v.value : v);

/// Customizer groups: [{name, parameters: [{name, description, control,
/// defaultValue}]}], defaults as plain JSON.
export function parameterGroups(groups) {
  return (groups ?? []).map((g) => ({
    name: String(g.name ?? ""),
    parameters: (g.parameters ?? []).map((p) => ({
      name: String(p.name),
      description: String(p.description ?? ""),
      control: control(p.control),
      defaultValue: plainValue(p.defaultValue),
    })),
  }));
}

const CONSOLE_KINDS = new Set(["error", "warning", "deprecated", "echo", "trace", "info"]);

/// Console lines: [{kind, text, location: {path, startLine,
/// startCharacter, endLine, endCharacter} | null}].
export function consoleLines(lines) {
  return (lines ?? []).map((l) => ({
    kind: CONSOLE_KINDS.has(l.kind) ? l.kind : "info",
    text: String(l.text ?? ""),
    location: l.location ?? null,
  }));
}

/// A run's result for the panels: {exitCode, geometry, timings, console,
/// language, scene, fileView, files}. `scene` is the packed scene
/// ({faces, edges, meta}) for the viewer, passed through untouched.
export function runResult(r) {
  const render = r?.render ?? {};
  return {
    exitCode: render.exitCode ?? 0,
    geometry: render.geometry ?? null,
    // The console's summary line and tooltip, worded by the core.
    summary: r?.summary ?? "",
    timingsText: r?.timingsText ?? "",
    timings: render.timings ?? null,
    console: consoleLines(r?.console),
    language: r?.language ?? [],
    scene: r?.scene ?? null,
    fileView: r?.fileView ?? null,
    files: r?.files ?? [],
    // Its text was drawn without the fonts, which the page fetches on
    // first use: add them and run again.
    fontsWanted: r?.fontsWanted === true,
  };
}

/// A check report with its lists always present.
export function checkReport(r) {
  return { ...r, findings: r?.findings ?? [], truncated: r?.truncated ?? [], parts: r?.parts ?? [] };
}

export const measureResult = (r) => ({ ...r, parts: r?.parts ?? [] });

/// A flat `[x0, y0, z0, x1, ...]` (a section's outline loop) as points.
export function triplets(flat) {
  const out = [];
  for (let i = 0; i + 2 < flat.length; i += 3) out.push([flat[i], flat[i + 1], flat[i + 2]]);
  return out;
}

/// A section with its outline loops as lists of points (the viewer's
/// annotation lines take points).
export const sectionResult = (r) => ({ ...r, outline: (r?.outline ?? []).map(triplets) });
export const betweenResult = (r) => r ?? {};

// --- Helpers the host and the tests share ------------------------------------

/// Whether a text includes or uses a library, so its files must be in the
/// worker before the run (they are fetched on first use; plan: "lazy
/// BOSL2"). Comments are not stripped: a commented-out include costs one
/// needless fetch, which is cheaper than a parser here.
export function usesLibrary(text, name) {
  const re = new RegExp(`\\b(?:include|use)\\s*<\\s*${name}/`);
  return re.test(text);
}

/// Whether a text draws or measures text, so the fonts should be in the
/// worker before its run. A guess that saves a run on a model's first
/// text: the core says when a run wanted fonts it did not have
/// (`fontsWanted`), which also covers text drawn in an included library.
export function usesText(text) {
  return /\b(?:text|textmetrics|fontmetrics)\s*\(/.test(text);
}

/// Whether two `fileView`s (the `$vp*` a file assigned) differ: the view
/// follows the file only when they do, so a live preview of a file that
/// sets `$vpr` does not undo the user's orbit at every keystroke.
export function fileViewChanged(a, b) {
  return JSON.stringify(a ?? null) !== JSON.stringify(b ?? null);
}
