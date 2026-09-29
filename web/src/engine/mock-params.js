// The mock engine's customizer: a rough reading of OpenSCAD's customizer
// comments, so the panel can be built and tested against the real
// examples before the wasm core exists. It is not the core's parser
// (crates/session reads them properly); it covers what the examples use:
//
//   /* [Group] */            starts a group ("Hidden" is left out)
//   // description          on the line before an assignment
//   name = 10; // [0:100]   slider (also [min:step:max] and [max])
//   name = 2;  // [1, 2, 3] dropdown (also [1:One, 2:Two] with labels)
//   name = 5;  // 0.5       spin box with that step
//   name = "s"; // 8        text of at most 8 characters
//   name = true;            checkbox
//   name = [1, 2, 3];       vector
//
// Only literal values count, and only top-level assignments before the
// first module or function, as in OpenSCAD's customizer. The shapes it
// returns are the wire's draft (snake_case keys, `kind`-tagged controls,
// plain values), so the front end's normalisers see what the worker is
// expected to send.

function literal(src) {
  const s = src.trim();
  if (s === "true") return true;
  if (s === "false") return false;
  if (/^-?(\d+\.?\d*|\.\d+)(e[-+]?\d+)?$/i.test(s)) return Number(s);
  const str = s.match(/^"((?:[^"\\]|\\.)*)"$/);
  if (str) return str[1].replace(/\\(.)/g, "$1");
  const vec = s.match(/^\[(.*)\]$/);
  if (vec) {
    const items = vec[1].split(",").map((x) => x.trim()).filter((x) => x.length);
    const nums = items.map(Number);
    if (items.length && nums.every(Number.isFinite)) return nums;
  }
  return undefined;
}

function optionValue(s, like) {
  const t = s.trim();
  const v = literal(t);
  if (v !== undefined) return typeof like === "string" && typeof v !== "string" ? String(v) : v;
  return t;
}

function controlFor(value, hint) {
  const h = (hint ?? "").trim();
  const bracket = h.match(/^\[(.*)\]$/);
  if (typeof value === "boolean") return { kind: "checkbox" };
  if (Array.isArray(value)) return { kind: "vector", min: null, max: null, step: null };
  if (bracket) {
    const body = bracket[1];
    if (body.includes(",")) {
      const options = body.split(",").map((part) => {
        const m = part.match(/^\s*([^:]+?)\s*:\s*(.+?)\s*$/);
        if (m && typeof value === "number") return { label: m[2], value: optionValue(m[1], value) };
        const v = optionValue(part, value);
        return { label: String(v), value: v };
      });
      return { kind: "dropdown", options };
    }
    const nums = body.split(":").map((x) => Number(x.trim()));
    if (typeof value === "number" && nums.every(Number.isFinite)) {
      if (nums.length === 1) return { kind: "slider", min: 0, max: nums[0], step: null };
      if (nums.length === 2) return { kind: "slider", min: nums[0], max: nums[1], step: null };
      if (nums.length === 3) return { kind: "slider", min: nums[0], max: nums[2], step: nums[1] };
    }
  }
  if (typeof value === "string") {
    const n = Number(h);
    return { kind: "text", max_length: h && Number.isInteger(n) && n > 0 ? n : null };
  }
  const step = Number(h);
  return { kind: "spin_box", min: null, max: null, step: h && Number.isFinite(step) && step > 0 ? step : null };
}

export function parseParameters(text) {
  const groups = [];
  const byName = new Map();
  let group = "Parameters";
  let description = "";
  let depth = 0;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (depth === 0) {
      if (/^(module|function)\b/.test(line)) break;
      const g = line.match(/^\/\*\s*\[\s*(.*?)\s*\]\s*\*\/$/);
      if (g) {
        group = g[1];
        description = "";
        continue;
      }
      const a = raw.match(/^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([^;]*);\s*(?:\/\/(.*))?$/);
      if (a) {
        const value = literal(a[2]);
        if (value !== undefined && group.toLowerCase() !== "hidden") {
          if (!byName.has(group)) {
            byName.set(group, { name: group, parameters: [] });
            groups.push(byName.get(group));
          }
          byName.get(group).parameters.push({
            name: a[1],
            description,
            control: controlFor(value, a[3]),
            default_value: value,
          });
        }
        description = "";
        continue;
      }
      const c = line.match(/^\/\/(.*)$/);
      description = c ? c[1].trim() : "";
    }
    for (const ch of raw.replace(/\/\/.*$/, "").replace(/"(?:[^"\\]|\\.)*"/g, "")) {
      if (ch === "{") depth++;
      else if (ch === "}") depth = Math.max(0, depth - 1);
    }
  }
  return groups;
}
