// Runs before the page's first paint: a classic script in <head>, which
// blocks rendering until it has run, and that is its whole purpose. It
// picks the embed view's layout (`#embed=1`, share.js) from the fragment.
//
// app.js is a module, and modules run after the browser has already
// painted the parsed HTML. When it was app.js (embed.js) that added the
// `embed` class, an embed first drew the whole page's top bar and panels
// and then moved the 3D view up over them: a layout shift of about 0.35
// on every blog post with an embed. The test is parseShare()'s
// (share.js), so the two agree on what an embed link is.
if (new URLSearchParams(location.hash.replace(/^#/, "")).get("embed") === "1") {
  document.documentElement.classList.add("embed");
}
