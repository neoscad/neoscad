// A run's raw result (docs/web-protocol.md, `run`) as the package hands it
// over: `{ok, exitCode, scene, console, errors, warnings, bbox, fileView,
// summary}`.
//
// Positions are given in one convention everywhere: 0-based lines and
// UTF-16 columns, end exclusive (the protocol's ConsoleLine, and the
// repo's editor-position rule), so `text.split("\n")[line].slice(column)`
// is where a message points. A Diagnostic's span is 1-based lines and
// 1-based *byte* columns; it is converted through the text of the file it
// names when the package has that text (the model or one of its `files`),
// since a byte column into a line with "é" in it is not a string index.
//
// Paths are relative to the model's directory ("main.scad",
// "lib/part.scad"); a library's file keeps its absolute in-memory path
// ("/neoscad/libraries/BOSL2/std.scad").

import { consoleLines } from "../../src/engine/protocol.js";

const PREFIX = /^(?:ERROR|WARNING|DEPRECATED|TRACE|ECHO): /;

/// The UTF-16 column of a 1-based byte column in `line`.
export function utf16Column(line, byteColumn) {
  const bytes = new TextEncoder().encode(line);
  const n = Math.max(0, Math.min(bytes.length, byteColumn - 1));
  return new TextDecoder().decode(bytes.subarray(0, n)).length;
}

/// A Diagnostic span as 0-based lines and UTF-16 columns, using `text` (the
/// file's source) when known; without it the byte columns are kept, minus
/// one, which is right for ASCII lines.
export function spanPosition(span, text) {
  if (!span) return null;
  const lines = text == null ? null : text.split("\n");
  const col = (l, c) => (lines && lines[l - 1] != null ? utf16Column(lines[l - 1], c) : Math.max(0, c - 1));
  return {
    line: span.startLine - 1,
    column: col(span.startLine, span.startColumn),
    endLine: span.endLine - 1,
    endColumn: col(span.endLine, span.endColumn),
  };
}

/// `path` as the page names it: relative to the model's directory `dir`
/// ("/doc/m1/") when inside it.
export const relativePath = (path, dir) => (path && dir && path.startsWith(dir) ? path.slice(dir.length) : path ?? null);

/// One message: `{severity, message, text, path, line, column, endLine,
/// endColumn, code, hints}`; `line` and the rest are null when the
/// message has no position.
function message(severity, { text, message, path, pos, code = null, hints = [] }) {
  return {
    severity,
    message: message ?? text.replace(PREFIX, "").trim(),
    text,
    path,
    line: pos?.line ?? null,
    column: pos?.column ?? null,
    endLine: pos?.endLine ?? null,
    endColumn: pos?.endColumn ?? null,
    code,
    hints,
  };
}

/// The package's result for a raw run result. `dir` is the model's
/// directory; `texts` maps absolute paths to the sources the package sent
/// (for converting Diagnostic columns).
export function shapeResult(raw, { dir = null, texts = new Map() } = {}) {
  const render = raw?.render ?? {};
  const console = consoleLines(raw?.console).map((l) => ({
    kind: l.kind,
    text: l.text,
    location: l.location
      ? {
          path: relativePath(l.location.path, dir),
          line: l.location.startLine,
          column: l.location.startCharacter,
          endLine: l.location.endLine,
          endColumn: l.location.endCharacter,
        }
      : null,
  }));
  const out = { error: [], warning: [] };
  const seen = new Set();
  for (const d of render.diagnostics ?? []) {
    const severity = d.severity === "error" ? "error" : "warning";
    seen.add(d.text);
    out[severity].push(
      message(severity, {
        text: d.text ?? d.message,
        message: d.message,
        path: relativePath(d.file, dir),
        pos: spanPosition(d.span, d.file ? texts.get(d.file) : null),
        code: d.code ?? null,
        hints: (d.hints ?? []).map((h) => h.message),
      }),
    );
  }
  // Console lines the diagnostics do not already cover (the same line
  // word for word): OpenSCAD's own errors and warnings from evaluation.
  for (const l of console) {
    const severity = l.kind === "error" ? "error" : l.kind === "warning" || l.kind === "deprecated" ? "warning" : null;
    if (!severity || seen.has(l.text)) continue;
    seen.add(l.text);
    out[severity].push(message(severity, { text: l.text, path: l.location?.path ?? null, pos: l.location }));
  }
  const scene = raw?.scene ?? null;
  const exitCode = render.exitCode ?? 0;
  return {
    ok: exitCode === 0 && out.error.length === 0,
    exitCode,
    summary: raw?.summary ?? "",
    scene,
    console,
    errors: out.error,
    warnings: out.warning,
    bbox: sceneBBox(scene),
    geometry: render.geometry ?? null,
    fileView: raw?.fileView ?? null,
    timings: render.timings ?? null,
  };
}

/// The model's box from the packed scene's meta (`{bbox: [min, max]}`);
/// a preview has no `render.geometry`, so this is the box for both modes.
export function sceneBBox(scene) {
  if (!scene?.meta) return null;
  try {
    const b = JSON.parse(scene.meta).bbox;
    return Array.isArray(b) && b.length === 2 ? { min: b[0], max: b[1] } : null;
  } catch {
    return null;
  }
}

/// Why an export produced no file.
export class ExportError extends Error {
  constructor(r) {
    super(r.failure ?? `export failed (exit code ${r.exitCode})`);
    this.name = "ExportError";
    this.failure = r.failure ?? null;
    this.exitCode = r.exitCode;
    this.messages = r.messages ?? [];
  }
}

/// An export's raw result as `{data, ok, exitCode, failure, mime, format,
/// messages}`. `ok` is false for a file written with errors (failed
/// fillets: the edges are sharp; docs/fillets.md section 18).
export function shapeExport(raw, { dir = null, texts = new Map() } = {}) {
  const messages = shapeResult({ render: { diagnostics: raw.diagnostics ?? [], exitCode: raw.exitCode } }, { dir, texts });
  const data = raw.data ? new Uint8Array(raw.data) : null;
  return {
    data,
    ok: data !== null && raw.exitCode === 0,
    exitCode: raw.exitCode,
    failure: raw.failure ?? null,
    mime: raw.mime ?? null,
    format: raw.format ?? null,
    messages: [...messages.errors, ...messages.warnings],
  };
}
