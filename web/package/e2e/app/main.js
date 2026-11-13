// An app that bundles the package (e2e/serve.mjs bundles this with
// esbuild). Bundled, `import.meta.url` is the app's, not the package's,
// so the default assets would be /app/assets/: the app passes the
// directory it serves them from, as the README tells it to.

import { VERSION, render } from "@neoscad/viewer";

const view = await render(document.getElementById("view"), "cube([4, 6, 8]);", { assetsUrl: "/pkg/dist/assets/" });
window.APP = { version: VERSION, ok: view.result.ok, bbox: view.result.bbox, assets: view.engine.assets.href, kind: view.viewer.kind };
