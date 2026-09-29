// The website's contract with this bundle (../neoscad-website: site.json
// and theme.css). The page reads the site's `site.json` for its name,
// home and nav links, and links the site's `theme.css`, whose custom
// properties (--ns-*) override the defaults in styles.css. Both are
// optional: served on its own the bundle keeps its defaults and a link
// to neoscad.org.
//
// Where site.json is comes from `<meta name="neoscad-site">`, relative to
// the page (default "../site.json": the bundle unpacks into /try/). Its
// hrefs are resolved against site.json's own URL, so root-relative ones
// ("/download.html") work on the site and relative ones anywhere.

export const DEFAULT_SITE = {
  name: "NeoSCAD",
  home: "https://neoscad.org/",
  theme: null,
  nav: [],
};

/// A site.json object checked and resolved against `base`.
export function resolveSite(json, base) {
  if (!json || typeof json !== "object" || json.schema !== 1) return DEFAULT_SITE;
  const url = (href) => {
    try {
      return new URL(String(href), base).href;
    } catch {
      return null;
    }
  };
  return {
    name: typeof json.name === "string" ? json.name : DEFAULT_SITE.name,
    home: url(json.home ?? "/") ?? DEFAULT_SITE.home,
    theme: json.theme ? url(json.theme) : null,
    nav: (Array.isArray(json.nav) ? json.nav : [])
      .filter((n) => n && typeof n.label === "string" && typeof n.href === "string")
      .map((n) => ({ label: n.label, href: url(n.href) }))
      .filter((n) => n.href),
  };
}

export async function loadSite(doc = document) {
  const where = doc.querySelector('meta[name="neoscad-site"]')?.content || "../site.json";
  const url = new URL(where, doc.baseURI);
  try {
    const res = await fetch(url, { cache: "no-cache" });
    if (!res.ok) return DEFAULT_SITE;
    return resolveSite(await res.json(), url);
  } catch {
    return DEFAULT_SITE;
  }
}

/// Link the site's theme after the bundle's own styles, so its tokens win.
export function applyTheme(site, doc = document) {
  if (!site.theme) return;
  const link = doc.createElement("link");
  link.rel = "stylesheet";
  link.href = site.theme;
  link.dataset.site = "theme";
  doc.head.append(link);
}
