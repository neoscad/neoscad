// A small element builder: `h("button", {class: "x", onclick}, "Label")`.
// Attributes starting with "on" become listeners; `dataset` and `style`
// objects are assigned; false, null and undefined attributes and
// children are skipped, so conditionals read naturally.

export function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs ?? {})) {
    if (v === false || v == null) continue;
    if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v);
    else if (k === "dataset") Object.assign(el.dataset, v);
    else if (k === "style" && typeof v === "object") Object.assign(el.style, v);
    else if (k === "class") el.className = v;
    else if (k in el && typeof v !== "string") el[k] = v;
    else el.setAttribute(k, v === true ? "" : v);
  }
  append(el, children);
  return el;
}

function append(el, children) {
  for (const c of children.flat(Infinity)) {
    if (c === false || c == null) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
}

export function clear(el, ...children) {
  el.replaceChildren();
  append(el, children);
  return el;
}

/// Save `bytes` as a download named `name`.
export function download(bytes, name, mime) {
  const url = URL.createObjectURL(new Blob([bytes], { type: mime }));
  const a = h("a", { href: url, download: name, style: { display: "none" } });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10000);
}

/// A number for display: up to 4 significant decimals, no trailing zeros.
export function fmt(x, digits = 4) {
  if (x == null || !Number.isFinite(x)) return "–";
  return String(Number(x.toFixed(digits)));
}

export const vec = (v) => (v ? `[${v.map((x) => fmt(x, 3)).join(", ")}]` : "–");
