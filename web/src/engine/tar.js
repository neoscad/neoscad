// Reading a tar archive (ustar, as `tar` and `COPYFILE_DISABLE=1 tar`
// write it), for the lazily fetched library bundles (bosl2.tar.gz, gunzipped
// with DecompressionStream). Regular files only; directories, links and
// pax headers are skipped, and a pax `path` record names the next file,
// which is how GNU and bsdtar store names longer than 100 bytes.

const dec = new TextDecoder();

function field(block, at, len) {
  const bytes = block.subarray(at, at + len);
  const end = bytes.indexOf(0);
  return dec.decode(end < 0 ? bytes : bytes.subarray(0, end));
}

function octal(block, at, len) {
  const s = field(block, at, len).trim();
  return s ? parseInt(s, 8) : 0;
}

/// `[{path, bytes}]` for every regular file, with `bytes` a Uint8Array
/// view into `data`.
export function untar(data) {
  const files = [];
  let at = 0;
  let longName = null;
  while (at + 512 <= data.length) {
    const block = data.subarray(at, at + 512);
    if (block.every((b) => b === 0)) break;
    const name = field(block, 0, 100);
    const size = octal(block, 124, 12);
    const type = String.fromCharCode(block[156] || 48);
    const prefix = field(block, 345, 155);
    const body = data.subarray(at + 512, at + 512 + size);
    at += 512 + Math.ceil(size / 512) * 512;
    if (type === "x") {
      const m = dec.decode(body).match(/\d+ path=([^\n]*)\n/);
      longName = m ? m[1] : null;
      continue;
    }
    if (type === "L") {
      longName = dec.decode(body).replace(/\0+$/, "");
      continue;
    }
    const path = longName ?? (prefix ? `${prefix}/${name}` : name);
    longName = null;
    if (type === "0" || type === "\0") files.push({ path: path.replace(/^\.\//, ""), bytes: body });
  }
  return files;
}

/// A tar archive of `[{path, bytes}]`, for the tests.
export function tar(files) {
  const enc = new TextEncoder();
  const blocks = [];
  for (const f of files) {
    const h = new Uint8Array(512);
    h.set(enc.encode(f.path).subarray(0, 100), 0);
    h.set(enc.encode("0000644\0"), 100);
    h.set(enc.encode(f.bytes.length.toString(8).padStart(11, "0") + "\0"), 124);
    h.set(enc.encode("        "), 148);
    h[156] = 48;
    h.set(enc.encode("ustar\0" + "00"), 257);
    let sum = 0;
    for (const b of h) sum += b;
    h.set(enc.encode(sum.toString(8).padStart(6, "0") + "\0 "), 148);
    blocks.push(h, f.bytes, new Uint8Array((512 - (f.bytes.length % 512)) % 512));
  }
  blocks.push(new Uint8Array(1024));
  const out = new Uint8Array(blocks.reduce((s, b) => s + b.length, 0));
  let at = 0;
  for (const b of blocks) {
    out.set(b, at);
    at += b.length;
  }
  return out;
}
