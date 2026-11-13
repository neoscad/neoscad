// Where the package's assets are (the wasm core, the viewers, the lazy
// fonts and BOSL2) and how its worker is started from them.
//
// The default is `./assets/` beside the bundle (`import.meta.url`), which
// works without a bundler: from node_modules served as is, and from a CDN.
// Bundlers do not follow computed URLs, so a bundled app copies
// dist/assets into its public directory and passes `assetsUrl`.
//
// Browsers refuse `new Worker(url)` for a URL on another origin (a CDN's,
// say), but a module worker started from a same-origin `blob:` script may
// import one: so when the assets are elsewhere the worker is a one-line
// blob that imports the core's worker.js, whose own `import.meta.url` is
// then the CDN's, so the wasm beside it resolves there. A page whose CSP
// forbids `blob:` workers self-hosts the assets, or passes `worker`.

// Not a literal in `new URL(...)`: Vite and webpack rewrite
// `new URL("literal", import.meta.url)` as an asset to bundle, and a
// directory is not one.
const DIR = "./assets/";

/// The default assets directory: beside the bundle that holds this code
/// (dist/index.js, so dist/assets/).
export const defaultAssets = () => new URL(DIR, import.meta.url);

/// The assets directory as an absolute URL ending in "/": `assetsUrl` when
/// given (a string resolved against the document, or a URL), else `base`.
export function resolveAssets(assetsUrl, base, documentURL = globalThis.document?.baseURI ?? globalThis.location?.href) {
  const url = assetsUrl == null ? new URL(base) : new URL(String(assetsUrl), documentURL ?? base);
  if (!url.pathname.endsWith("/")) url.pathname += "/";
  url.search = "";
  url.hash = "";
  return url;
}

/// Whether a worker at `workerUrl` must be started through a blob: module
/// because the page (at `pageOrigin`) may not start it directly: another
/// origin, or a scheme a worker script cannot be loaded from.
export function needsBlobWorker(workerUrl, pageOrigin = globalThis.location?.origin) {
  const url = new URL(workerUrl);
  if (url.protocol === "blob:" || url.protocol === "data:") return false;
  if (pageOrigin == null || pageOrigin === "null") return true;
  return url.origin !== pageOrigin;
}

/// The source of the blob: module that loads the worker at `workerUrl`.
export const blobWorkerSource = (workerUrl) => `import ${JSON.stringify(String(workerUrl))};\n`;

/// A spawner for the worker at `workerUrl`: `{spawn, dispose}`. `spawn`
/// makes a new worker every call (the engine client respawns after a
/// crash or a cancel); the blob URL, when one is needed, is made once and
/// revoked by `dispose`.
export function workerSpawner(workerUrl, { pageOrigin = globalThis.location?.origin } = {}) {
  let src = String(workerUrl);
  let blob = null;
  if (needsBlobWorker(workerUrl, pageOrigin)) {
    blob = URL.createObjectURL(new Blob([blobWorkerSource(workerUrl)], { type: "text/javascript" }));
    src = blob;
  }
  return {
    url: String(workerUrl),
    viaBlob: blob !== null,
    spawn: () => new Worker(src, { type: "module", name: "neoscad-engine" }),
    dispose() {
      if (blob) URL.revokeObjectURL(blob);
      blob = null;
    },
  };
}

/// What to tell a page whose worker never started: almost always the
/// assets' URL is wrong, or CORS or a Content-Security-Policy blocks them.
export function startFailure(assets, viaBlob, reason) {
  const csp = viaBlob
    ? "the page's Content-Security-Policy must allow `worker-src blob:` and `script-src` and `connect-src` for that origin, with 'wasm-unsafe-eval'"
    : "the page's Content-Security-Policy must allow the worker (`worker-src`) and 'wasm-unsafe-eval'";
  return (
    `NeoSCAD's engine could not start from ${assets.href}core/worker.js (${reason}). ` +
    `Check that the assets are served there (a bundled app copies node_modules/@neoscad/viewer/dist/assets and passes assetsUrl), ` +
    `that a server on another origin sends Access-Control-Allow-Origin, and that ${csp}. ` +
    `Self-hosting the assets, or the \`worker\` option, avoids the cross-origin worker.`
  );
}
