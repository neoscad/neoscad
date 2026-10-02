// Models in the address: the source of a document carried in the page's
// fragment, so a link (a blog post's "Open in NeoSCAD", the Export menu's
// "Copy link") opens it, and `#embed=1` shows it in a bare live view for
// an iframe. The pure pieces, apart from the DOM so node can test them
// (test/share.test.js).
//
// The fragment's parameters (web/README.md, "Links and embeds", has the
// same table for people writing links):
//
//   code=<payload>   the source: base64url (RFC 4648 §5, no padding) of
//                    its UTF-8 bytes, or `z:` and the base64url of those
//                    bytes deflated (raw DEFLATE, RFC 1951: the browser's
//                    CompressionStream("deflate-raw"))
//   name=<file>      the document's file name (default untitled.scad)
//   example=<id>     a bundled example instead (examples/manifest.json)
//   embed=1          the embed view (embed.js) rather than the whole page
//
// It is the fragment, not the query, because the browser never sends a
// fragment to the server: a model shared this way stays between the
// people who have the link.

/// The most source a link may carry, decoded. A fragment this long is
/// still well within what browsers keep in an address (all of them take
/// several hundred KB), and it is far more than a blog's example needs;
/// the bound is what stops a crafted link from making the page inflate
/// a "zip bomb" into memory.
export const MAX_SOURCE_BYTES = 64 * 1024;

/// The longest payload worth decoding: plain base64url of the largest
/// source, plus the marker. Anything longer cannot be valid, so it is
/// refused before any work.
const MAX_PAYLOAD = Math.ceil((MAX_SOURCE_BYTES * 4) / 3) + 2;

export const DEFAULT_NAME = "untitled.scad";

/// A link's model that could not be opened: the message is for the user.
export class ShareError extends Error {}

/// What a fragment asks for: `{code, name, example, embed}`, with `code`
/// the undecoded payload (null without one) and `name` checked.
export function parseShare(hash) {
  const p = new URLSearchParams(String(hash ?? "").replace(/^#/, ""));
  return {
    code: p.has("code") ? p.get("code") : null,
    name: fileName(p.get("name")),
    example: p.get("example"),
    embed: p.get("embed") === "1",
  };
}

/// A fragment without the share parameters (`code`, `name`, `embed`),
/// "" when nothing is left. `example` stays: it is the page's own record
/// of what is open, not a payload. A fragment without them comes back as
/// it was (URLSearchParams would turn `#agent` into `#agent=`).
export function stripShare(hash) {
  const p = new URLSearchParams(String(hash ?? "").replace(/^#/, ""));
  const keys = ["code", "name", "embed"];
  if (!keys.some((k) => p.has(k))) return String(hash ?? "");
  for (const k of keys) p.delete(k);
  const rest = p.toString();
  return rest ? `#${rest}` : "";
}

/// A file name from a link, made safe to be a path in the worker's file
/// system (`/doc/<name>`) and a download's name: its last path segment,
/// no control characters, at most 64 characters, ending in `.scad`. A
/// name that would not survive that is the default.
export function fileName(name) {
  let s = String(name ?? "")
    .split(/[/\\]/)
    .pop()
    // Control characters and the ones a download name or URI would choke on.
    .replace(/[\u0000-\u001f\u007f<>:"|?*]/g, "")
    .trim();
  if (!s || s === "." || s === "..") return DEFAULT_NAME;
  if (!/\.scad$/i.test(s)) s += ".scad";
  if (s.length > 64) s = s.slice(0, 59).trim() + ".scad";
  return s.startsWith(".") ? DEFAULT_NAME : s;
}

// --- base64url ---

export function toBase64url(bytes) {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function fromBase64url(text) {
  // Padding is tolerated (some encoders add it), anything else is not:
  // atob() would otherwise skip whitespace and accept "+/" silently.
  const s = String(text).replace(/=+$/, "");
  if (!/^[A-Za-z0-9_-]*$/.test(s) || s.length % 4 === 1) throw new ShareError("it is not base64url");
  const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/"));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// --- Compression ---
//
// `streams` is how to make the (de)compressing transform: the browser's
// CompressionStream and DecompressionStream by default; the unit tests
// pass node:zlib's on a node without "deflate-raw".

const browserStreams = {
  compress: () => new CompressionStream("deflate-raw"),
  decompress: () => new DecompressionStream("deflate-raw"),
};

async function deflate(bytes, streams) {
  const stream = new Blob([bytes]).stream().pipeThrough(streams.compress());
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

/// Inflate, stopping as soon as the output passes `max` bytes: a small
/// payload can inflate to gigabytes, so the output is never collected
/// past the bound.
async function inflate(bytes, max, streams) {
  let transform;
  try {
    transform = streams.decompress();
  } catch {
    throw new ShareError("this browser cannot decompress it (it has no deflate-raw)");
  }
  const reader = new Blob([bytes]).stream().pipeThrough(transform).getReader();
  const chunks = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.length;
      if (total > max) throw new ShareError(`it is longer than ${max / 1024} KB`);
      chunks.push(value);
    }
  } catch (e) {
    reader.cancel().catch(() => {});
    if (e instanceof ShareError) throw e;
    throw new ShareError("its compressed data is damaged");
  }
  const out = new Uint8Array(total);
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}

/// The payload for `text`: compressed when that is shorter (it nearly
/// always is past a few lines), plain otherwise. Throws a ShareError
/// when the text is over the bound, since such a link would not open.
export async function encodeSource(text, { streams = browserStreams } = {}) {
  const bytes = new TextEncoder().encode(text);
  if (bytes.length > MAX_SOURCE_BYTES) {
    throw new ShareError(`the source is ${Math.ceil(bytes.length / 1024)} KB; a link can carry ${MAX_SOURCE_BYTES / 1024} KB`);
  }
  const plain = toBase64url(bytes);
  let packed = null;
  try {
    packed = `z:${toBase64url(await deflate(bytes, streams))}`;
  } catch {
    // No compression in this browser: the plain form still works.
  }
  return packed && packed.length < plain.length ? packed : plain;
}

/// The source in a payload. Throws a ShareError saying what is wrong
/// with it: too long, not base64url, damaged, or not UTF-8.
export async function decodeSource(payload, { streams = browserStreams, max = MAX_SOURCE_BYTES } = {}) {
  const s = String(payload ?? "");
  if (s.length > MAX_PAYLOAD) throw new ShareError(`it is longer than ${max / 1024} KB`);
  const compressed = s.startsWith("z:");
  let bytes = fromBase64url(compressed ? s.slice(2) : s);
  if (compressed) {
    bytes = await inflate(bytes, max, streams);
  } else if (bytes.length > max) {
    throw new ShareError(`it is longer than ${max / 1024} KB`);
  }
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new ShareError("it is not UTF-8 text");
  }
}

/// The fragment that opens `text` (`embed` for the embed view). `name`
/// is left out when it is the default.
export async function shareHash(text, { name = DEFAULT_NAME, embed = false, streams } = {}) {
  const code = await encodeSource(text, { streams });
  const n = fileName(name);
  return `#${embed ? "embed=1&" : ""}code=${code}${n === DEFAULT_NAME ? "" : `&name=${encodeURIComponent(n)}`}`;
}

/// The fragment that opens a bundled example.
export const exampleHash = (id, { embed = false } = {}) => `#${embed ? "embed=1&" : ""}example=${encodeURIComponent(id)}`;

/// Whether the page is framed, and by whom: "top" (not framed), "same"
/// (every frame above it is this origin), or "cross". A cross-origin
/// parent's location cannot be read (it throws), and neither can any
/// parent's from a sandboxed frame without allow-same-origin, whose origin
/// is opaque: both count as cross.
export function framing(win = globalThis) {
  try {
    if (win.top === win.self) return "top";
    const origin = win.location.origin;
    for (let w = win; w !== w.top; ) {
      w = w.parent;
      if (w.location.origin !== origin) return "cross";
    }
    // Chrome and Safari also list the ancestors' origins; a parent can
    // navigate itself after framing, which this sees too.
    const ancestors = win.location.ancestorOrigins;
    if (ancestors && [...ancestors].some((o) => o !== origin)) return "cross";
    return "same";
  } catch {
    return "cross";
  }
}
