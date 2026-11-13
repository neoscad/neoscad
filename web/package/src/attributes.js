// `<neoscad-viewer>`'s attributes as options: plain functions, so node
// tests them without a DOM.

export const OBSERVED = [
  "src",
  "mode",
  "enable",
  "color-scheme",
  "axes",
  "scales",
  "grid",
  "edges",
  "crosshairs",
  "orthographic",
  "view",
  "assets-url",
  "backend",
  "name",
];

/// Attributes that only change how the model is drawn, not what it is:
/// changing one updates the viewer without running the model again.
export const VIEW_ONLY = new Set(["axes", "scales", "grid", "edges", "crosshairs", "orthographic", "view"]);

const SETTINGS = ["axes", "scales", "grid", "edges", "crosshairs", "orthographic"];
// The names both viewers know (crates/web-view's setView, canvas2d.js).
const PRESETS = new Set(["top", "bottom", "left", "right", "front", "back", "diagonal"]);

/// A boolean attribute: absent is `fallback`; present is true unless it
/// says "false", "0", "off" or "no" (so `axes="false"` turns off what is
/// on by default).
export function flag(value, fallback) {
  if (value == null) return fallback;
  return !/^(false|0|off|no)$/i.test(String(value).trim());
}

/// `get(name)` (getAttribute) as `{src, mode, enable, colorScheme,
/// settings, view, assetsUrl, backend, name}`.
export function parseAttributes(get, defaults) {
  const mode = (get("mode") ?? "preview").trim().toLowerCase();
  const settings = {};
  for (const k of SETTINGS) {
    const v = flag(get(k), undefined);
    if (v !== undefined) settings[k] = v;
  }
  const scheme = get("color-scheme");
  if (scheme) settings.scheme = scheme.trim();
  const view = (get("view") ?? "").trim().toLowerCase();
  const backend = (get("backend") ?? "auto").trim().toLowerCase();
  return {
    src: get("src") ?? null,
    mode: mode === "render" ? "render" : "preview",
    enable: (get("enable") ?? "").split(/[\s,]+/).filter(Boolean),
    settings: { ...defaults, ...settings },
    view: PRESETS.has(view) ? view : null,
    assetsUrl: get("assets-url") || null,
    backend: ["auto", "webgpu", "webgl", "canvas2d"].includes(backend) ? backend : "auto",
    name: get("name") || null,
  };
}

/// The model's file name for a `src` URL: its last path segment when it
/// is a plain name ending in .scad, else main.scad. Includes resolve
/// against it, and errors name it.
export function nameFromSrc(src) {
  if (!src) return "main.scad";
  let path;
  try {
    path = new URL(src, "http://x/").pathname;
  } catch {
    return "main.scad";
  }
  const base = decodeURIComponent(path.split("/").pop() ?? "");
  return /^[\w.-]+\.scad$/i.test(base) && base !== ".scad" ? base : "main.scad";
}

/// Inline source: the text of the element's `<script type="text/x-openscad">`
/// (or "text/openscad"), with the blank first line and the common leading
/// indentation of HTML around it removed. Positions in results refer to
/// this text, which the element exposes as `source`.
export function inlineSource(text) {
  if (text == null) return null;
  const lines = String(text).replace(/^\s*\n/, "").replace(/\s+$/, "").split("\n");
  const indents = lines.filter((l) => l.trim()).map((l) => l.match(/^[ \t]*/)[0].length);
  const cut = indents.length ? Math.min(...indents) : 0;
  return lines.map((l) => l.slice(cut)).join("\n") + "\n";
}

export const isSourceScript = (el) =>
  el?.tagName === "SCRIPT" && /^text\/(x-)?openscad$/i.test((el.getAttribute("type") ?? "").trim());
