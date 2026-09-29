// The mock engine: the worker protocol (protocol.js) answered with canned
// results, so the front end can be built and tested before the wasm core
// (crates/web) exists. It keeps the documents' text, reads customizer
// parameters roughly (mock-params.js), turns `echo(...)` calls into
// console lines with locations (for click-to-jump), and returns a packed
// scene of boxes (fixtures.js).
//
// Two markers in a document's text drive the failure paths in tests:
//   // mock:slow=MS   the run busy-waits MS milliseconds (cancel, stale runs)
//   // mock:crash     the run throws, which the worker reports as fatal
//
// It is plain logic with no worker globals, so node tests use it directly
// and mock-worker.js wraps it.

import { boxMesh, mockScene, off, sceneTransfer, stl, svg, threeMF } from "./fixtures.js";
import { parseParameters } from "./mock-params.js";
import { applyEdits } from "./protocol.js";

export const MOCK_VERSION = "mock-0";

/// UTF-16 line/character of an offset.
function position(text, at) {
  const before = text.slice(0, at);
  const line = before.split("\n").length - 1;
  return { line, character: at - (before.lastIndexOf("\n") + 1) };
}

function busyWait(ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    // A real run holds the worker's thread the same way.
  }
}

export class MockCore {
  constructor() {
    this.files = new Map();
    this.lastScene = null;
    this.lastPath = null;
  }

  /// One request: `{result, transfer}`, or a thrown Error for `ok: false`.
  /// `notify(msg)` sends a message with no id (language server output).
  handle(msg, notify = () => {}) {
    switch (msg.type) {
      case "init":
        return { result: { engine: "mock", version: MOCK_VERSION, protocol: 0, features: ["part"] } };
      case "open":
        this.files.set(msg.path, msg.text);
        return { result: {} };
      case "edit": {
        const old = this.files.get(msg.path) ?? "";
        this.files.set(msg.path, msg.text ?? applyEdits(old, msg.edits ?? []));
        return { result: {} };
      }
      case "addFiles":
        for (const f of msg.files ?? []) this.files.set(f.path, f.text ?? f.bytes);
        return { result: { added: (msg.files ?? []).length } };
      case "read": {
        const f = this.files.get(msg.path);
        if (f === undefined) throw new Error(`no such file: ${msg.path}`);
        return { result: { text: typeof f === "string" ? f : new TextDecoder().decode(f) } };
      }
      case "parameters":
        return { result: { groups: parseParameters(this.text(msg.path)) } };
      case "run":
        return this.run(msg);
      case "check":
        return { result: this.check(msg) };
      case "measure":
        return { result: this.measure(msg) };
      case "section":
        return {
          result: {
            plane: `${msg.axis}=${msg.offset}`,
            area: 400,
            perimeter: 80,
            contours: 1,
            bbox_min: [-10, -10, msg.offset],
            bbox_max: [10, 10, msg.offset],
            outline: [[-10, -10, msg.offset, 10, -10, msg.offset, 10, 10, msg.offset, -10, 10, msg.offset]],
          },
        };
      case "between":
        return {
          result: {
            a: msg.a,
            b: msg.b,
            distance: 4,
            touching: false,
            overlapping: false,
            overlap_volume: 0,
            point_a: [10, 0, 3],
            point_b: [14, 0, 3],
          },
        };
      case "pick":
        return { result: { point: [0, 0, 10], part: null } };
      case "export":
        return this.export(msg);
      case "cancel":
        return { result: {} };
      case "lsp":
        this.lsp(msg.message, notify);
        return null;
      default:
        throw new Error(`unknown request: ${msg.type}`);
    }
  }

  text(path) {
    const t = this.files.get(path);
    if (typeof t !== "string") throw new Error(`not open: ${path}`);
    return t;
  }

  run(msg) {
    const text = this.text(msg.path);
    const slow = text.match(/\/\/\s*mock:slow=(\d+)/);
    if (slow) busyWait(Math.min(60000, Number(slow[1])));
    if (/\/\/\s*mock:crash/.test(text)) throw new MockCrash("mock:crash in the text");
    const console = [
      { kind: "info", text: "Mock engine: this build has no wasm core, so results are canned.", location: null },
    ];
    const re = /echo\s*\(([^;]*)\)\s*;/g;
    for (let m; (m = re.exec(text)); ) {
      const a = position(text, m.index);
      const b = position(text, m.index + m[0].length);
      console.push({
        kind: "echo",
        text: `ECHO: ${m[1].trim()}`,
        location: {
          path: msg.path,
          start_line: a.line,
          start_character: a.character,
          end_line: b.line,
          end_character: b.character,
        },
      });
    }
    if (/\bpart\s*\(/.test(text) && !msg.parts) {
      console.push({ kind: "warning", text: "WARNING: Ignoring unknown module 'part'", location: null });
    }
    const scene = mockScene(text, msg.mode);
    this.lastScene = mockScene(text, msg.mode);
    this.lastPath = msg.path;
    const b = scene.bbox;
    const geometry =
      msg.mode === "preview"
        ? null
        : {
            dimensions: 3,
            bbox_min: b.min,
            bbox_max: b.max,
            area: 2400,
            volume: 8000,
            triangles: scene.meshes.length * 12,
            vertices: scene.meshes.length * 8,
            manifold: true,
            components: scene.meshes.length,
            contours: null,
          };
    const ms = 1 + (slow ? Number(slow[1]) : 0);
    return {
      result: {
        render: {
          exit_code: 0,
          geometry,
          timings: { parse_ms: 0.1, evaluate_ms: 0.4, geometry_ms: ms - 0.5, total_ms: ms },
        },
        console,
        language: [],
        scene,
      },
      transfer: sceneTransfer(scene),
    };
  }

  parts(msg) {
    const text = this.text(msg.path);
    return msg.parts ? [...text.matchAll(/\bpart\s*\(\s*"([^"]+)"/g)].map((m) => m[1]) : [];
  }

  check(msg) {
    return {
      exit_code: 0,
      failed: false,
      errors: 0,
      warnings: 1,
      info: 1,
      findings: [
        {
          id: 1,
          severity: "warning",
          code: "thin-wall",
          message: "Wall 0.6 mm thick (the minimum is 0.8 mm)",
          part: this.parts(msg)[0] ?? null,
          point: [5, 0, 2],
          bbox_min: [4, -2, 0],
          bbox_max: [6, 2, 4],
          fix: "Thicken the wall to at least 0.8 mm.",
          value: 0.6,
          limit: 0.8,
        },
        {
          id: 2,
          severity: "info",
          code: "overhang",
          message: "Overhang of 50° over 12 mm²",
          part: null,
          point: [0, 0, 10],
          bbox_min: [-3, -3, 10],
          bbox_max: [3, 3, 10],
          fix: "Chamfer the edge to 45° or add support.",
          value: 50,
          limit: 45,
        },
      ],
      truncated: [],
      min_wall: 0.6,
      parts: this.parts(msg),
      text: "check: 1 warning, 1 note",
    };
  }

  measure(msg) {
    const solid = {
      volume: 8000,
      area: 2400,
      bbox_min: [-10, -10, 0],
      bbox_max: [10, 10, 20],
      centroid: [0, 0, 10],
      triangles: 12,
    };
    return {
      exit_code: 0,
      model: solid,
      components: 1,
      manifold: true,
      model_2d: null,
      parts: this.parts(msg).map((name) => ({ name, instances: 1, context: null, solid })),
    };
  }

  export(msg) {
    const scene = this.lastScene ?? { meshes: [boxMesh([0, 0, 0], [10, 10, 10])] };
    const make = { stl: () => stl(scene), off: () => off(scene), svg, "3mf": () => threeMF(scene) }[msg.format];
    if (!make) throw new Error(`unknown export format: ${msg.format}`);
    const bytes = make();
    return { result: { format: msg.format, bytes: bytes.buffer }, transfer: [bytes.buffer] };
  }

  /// Enough of a language server for the editor's client to initialise
  /// and not wait on its requests: `initialize` gets capabilities, any
  /// other request a null result, notifications nothing.
  lsp(message, notify) {
    let m;
    try {
      m = JSON.parse(message);
    } catch {
      return;
    }
    if (m.id === undefined || m.method === undefined) return;
    const result =
      m.method === "initialize"
        ? { capabilities: { positionEncoding: "utf-16", textDocumentSync: { openClose: true, change: 2 } } }
        : null;
    notify({ type: "lsp", message: JSON.stringify({ jsonrpc: "2.0", id: m.id, result }) });
  }
}

/// A failure that kills the instance, as a wasm trap does.
export class MockCrash extends Error {}
