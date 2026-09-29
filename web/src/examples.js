// The example picker's data: examples/manifest.json beside the bundle and
// each example's text, fetched relative to the page so the bundle works
// under any sub-path.

export async function loadManifest(base = document.baseURI) {
  const res = await fetch(new URL("./examples/manifest.json", base));
  if (!res.ok) throw new Error(`examples/manifest.json: HTTP ${res.status}`);
  return checkManifest(await res.json());
}

/// The manifest with its fields checked: an example without an id, a
/// title or a file is dropped rather than breaking the picker.
export function checkManifest(m) {
  const examples = (m?.examples ?? []).filter(
    (e) => typeof e.id === "string" && typeof e.title === "string" && typeof e.file === "string" && !e.file.includes(".."),
  );
  const fallback = examples[0]?.id ?? null;
  return {
    default: examples.some((e) => e.id === m?.default) ? m.default : fallback,
    examples: examples.map((e) => ({
      parts: false,
      heavy: false,
      autorun: !e.heavy,
      note: "",
      ...e,
    })),
  };
}

export async function loadExampleText(example, base = document.baseURI) {
  const res = await fetch(new URL(`./examples/${example.file}`, base));
  if (!res.ok) throw new Error(`${example.file}: HTTP ${res.status}`);
  return res.text();
}
