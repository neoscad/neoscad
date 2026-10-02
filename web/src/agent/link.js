// The connect link of `neoscad mcp --browser` (crates/cli/src/mcp/bridge.rs)
// and the other pure pieces of the agent connection, apart from the DOM so
// node can test them (test/agent.test.js).
//
// A link is the page with `#connect=PORT.TOKEN`: the bridge's port on
// 127.0.0.1 and its 128-bit token in hex. It is in the fragment, which the
// browser never sends to the site, and the page takes it out of the
// address bar as soon as it has read it.

/// `{port, token}` from a link, a fragment or a bare `PORT.TOKEN` (what a
/// user may paste), or null.
export function parseConnect(text) {
  const s = String(text ?? "").trim();
  const m = s.match(/(?:^|[#&?]connect=)(\d{1,5})\.([0-9a-f]{32})(?:$|&|\s)/i) ?? s.match(/^(\d{1,5})\.([0-9a-f]{32})$/i);
  if (!m) return null;
  const port = Number(m[1]);
  if (!(port > 0 && port < 65536)) return null;
  return { port, token: m[2].toLowerCase() };
}

/// A fragment without its `connect=` (and `via=`) parameters (the rest,
/// such as `example=`, stays): "" when nothing is left.
export function stripConnect(hash) {
  const params = new URLSearchParams(String(hash ?? "").replace(/^#/, ""));
  params.delete("connect");
  params.delete("via");
  const rest = params.toString();
  return rest ? `#${rest}` : "";
}

export const wsURL = ({ port, token }) => `ws://127.0.0.1:${port}/ws?token=${token}`;
export const relayURL = ({ port, token }) => `http://127.0.0.1:${port}/relay#${token}`;
export const relayOrigin = ({ port }) => `http://127.0.0.1:${port}`;
export const linkText = ({ port, token }) => `${port}.${token}`;

/// A browser's name for the agent's "connected: ... in Firefox 155".
export function browserName(ua = "") {
  const v = (re) => ua.match(re)?.[1] ?? "";
  if (/Edg\//.test(ua)) return `Edge ${v(/Edg\/(\d+)/)}`.trim();
  if (/Firefox\//.test(ua)) return `Firefox ${v(/Firefox\/(\d+)/)}`.trim();
  if (/Chrome\//.test(ua)) return `Chrome ${v(/Chrome\/(\d+)/)}`.trim();
  if (/Safari\//.test(ua)) return `Safari ${v(/Version\/(\d+)/)}`.trim();
  return "a browser";
}

/// The names of the permission that lets an https page reach 127.0.0.1,
/// newest first: Chrome 145 split "local-network-access" (Chrome 142-144,
/// still an alias) into "local-network" and "loopback-network", and only
/// the loopback one matters for the bridge.
export const LOOPBACK_PERMISSIONS = ["loopback-network", "local-network-access"];

/// Whether the browser says it will refuse the page a connection to
/// 127.0.0.1 (the user said no to Chrome's or Edge's prompt before). The
/// direct attempt would then fail at once, and only after the page has
/// shown "Connecting…" and told the user to choose Allow in a prompt that
/// never comes. A name the browser does not know makes `query` throw
/// (Firefox before 153, Safari), and no answer at all counts as not
/// denied: the direct attempt is still the one to try.
export async function loopbackDenied(permissions = globalThis.navigator?.permissions) {
  for (const name of LOOPBACK_PERMISSIONS) {
    try {
      const status = await permissions.query({ name });
      return status?.state === "denied";
    } catch {
      // Not this browser's name for it; try the next.
    }
  }
  return false;
}

/// How to let the page connect directly next time, for Chrome and Edge
/// (the browsers whose site settings have the switch), or null. Chrome 145
/// renamed the setting "Local network access" to "Apps on device" when it
/// split it.
export function allowDirectHint(ua = "") {
  if (!/Chrome\/|Edg\//.test(ua) || /Firefox\/|OPR\//.test(ua)) return null;
  const major = Number(ua.match(/(?:Edg|Chrome)\/(\d+)/)?.[1] ?? 0);
  const setting = major && major < 145 ? "Local network access" : "Apps on device";
  return `To connect directly next time: click the icon at the left of the address bar, choose Site settings, and set “${setting}” to Allow.`;
}

/// A console line (the panels' `{kind, text, location}`) as the agent
/// reads it: 1-based line, and the file only when it is not the document.
export function agentLine(l, docPath) {
  const out = { kind: l.kind, text: l.text };
  if (l.location) {
    out.line = (l.location.startLine ?? 0) + 1;
    if (l.location.path && l.location.path !== docPath) out.file = l.location.path.split("/").pop();
  }
  return out;
}

/// The page's diagnostics for `editor_read`: its console's errors and
/// warnings.
export const diagnostics = (lines, docPath) =>
  lines.filter((l) => l.kind === "error" || l.kind === "warning").map((l) => agentLine(l, docPath));

/// Width and height of a capture whose longest side is at most `size`,
/// in the view's proportions (and never larger than the view itself).
export function captureSize(width, height, size) {
  const w = Math.max(1, width);
  const h = Math.max(1, height);
  const scale = Math.min(1, size / Math.max(w, h));
  return [Math.max(1, Math.round(w * scale)), Math.max(1, Math.round(h * scale))];
}

/// Bytes as base64 (the capture's PNG), in chunks so a large image does
/// not overflow the argument list of `String.fromCharCode`.
export function toBase64(bytes) {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}
