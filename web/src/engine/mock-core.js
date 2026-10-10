// The mock engine: the worker protocol (docs/web-protocol.md) answered with
// canned results, for developing the page without building the wasm core
// (`npm run build`) and for the unit tests. It speaks the real wire: the
// request names and fields, camelCase results, tagged parameter values, a
// packed scene, error kinds and the `crashed` message. It keeps the
// documents' text, reads customizer parameters roughly (mock-params.js),
// turns `echo(...)` calls into console lines with locations (for
// click-to-jump), and draws boxes (fixtures.js).
//
// Two markers in a document's text drive the failure paths in tests:
//   // mock:slow=MS   the run busy-waits MS milliseconds (cancel, stale runs)
//   // mock:crash     the run traps, which the worker reports as `crashed`
//
// It is plain logic with no worker globals, so node tests use it directly
// and mock-worker.js wraps it.

import { mockModel, off, packScene, stl, svg, threeMF, boxMesh } from "./fixtures.js";
import { parseParameters } from "./mock-params.js";
import { offsetAt, positionAt } from "./protocol.js";
import { untar } from "./tar.js";

export const MOCK_VERSION = "0.0.0-mock";

const MIME = { stl: "model/stl", binstl: "model/stl", "3mf": "model/3mf", off: "text/plain", svg: "image/svg+xml", step: "model/step" };

/// What the core reports for a STEP export (`client::StepReport`), fixed:
/// one exact curve and one faceted region, so the page's lines can be
/// tested without the wasm core.
const MOCK_STEP_REPORT = {
  ok: true,
  error: null,
  faces: 16,
  exactFaces: 7,
  exactPercent: 43.75,
  exactCurves: 1,
  polygons: 0,
  partial: null,
  fallback: null,
  facetedRegions: [{ module: "hull", file: "CSG.scad", line: 2, count: 1, detail: "is exported as planar facets: hull() has no exact surfaces in STEP export yet" }],
  summary: "STEP: 7 of 16 faces exact (43.8%).\n1 curve made exact.\nFaceted: hull() at CSG.scad, line 2 is exported as planar facets",
};

/// The modules each NeoSCAD extension adds (docs/language-extensions.md);
/// `query`'s are functions, which the mock does not look for.
const EXTENSION_MODULES = { sketch: ["sketch"], fillet: ["fillet_edges", "chamfer_edges"] };
const CHECK_DEFAULTS ={ nozzle: 0.4, minWall: 0.8, maxOverhang: 45, bed: null, bedTolerance: 0.5, maxFindings: 50 };
const LIMITS = {
  timeSeconds: 60,
  memoryBytes: 1 << 30,
  fragments: 10000,
  slices: 10000,
  list: 10000000,
  string: 67108864,
  rands: 10000000,
  triangles: 10000000,
  sketchUnknowns: 5000,
  queries: 10000,
};

function busyWait(ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    // A real run holds the worker's thread the same way.
  }
}

/// An `ok: false` reply's error.
export class MockError extends Error {
  constructor(kind, message) {
    super(message);
    this.kind = kind;
  }
}

/// A failure that kills the instance, as a wasm trap does.
export class MockCrash extends Error {}

const invalid = (m) => new MockError("invalidArgument", m);

export class MockCore {
  constructor() {
    this.files = new Map();
    this.versions = new Map();
    this.model = null;
    this.measurement = 0;
    this.initialised = false;
    this.lspOpen = new Set();
  }

  /// One request: `{result, transfer}`, or a thrown MockError (`ok:
  /// false`) or MockCrash (the instance traps).
  handle(msg) {
    if (msg.type === "init") {
      if (this.initialised) throw invalid("the worker is already initialised");
      this.initialised = true;
      return { result: { version: MOCK_VERSION, limits: LIMITS, libraryDirs: ["/neoscad/libraries"] } };
    }
    if (!this.initialised) throw invalid(`'${msg.type}' before 'init'`);
    switch (msg.type) {
      case "defaults":
        return { result: { checkOptions: CHECK_DEFAULTS, limits: LIMITS, tables: { previewDelayMs: 150 } } };
      case "stats":
        return { result: { memoryBytes: 16 << 20 } };
      case "open":
        if (msg.text != null) this.files.set(msg.path, msg.text);
        else if (!this.files.has(msg.path)) throw new MockError("failed", `no such file: ${msg.path}`);
        return { result: this.docInfo(msg.path) };
      case "update":
        this.files.set(msg.path, msg.text);
        return { result: this.docInfo(msg.path) };
      case "edit": {
        let text = this.text(msg.path);
        for (const e of msg.edits ?? []) {
          const a = offsetAt(text, e.start);
          const b = offsetAt(text, e.end);
          if (b < a) throw invalid("an edit's end is before its start");
          text = text.slice(0, a) + e.text + text.slice(b);
        }
        this.files.set(msg.path, text);
        return { result: this.docInfo(msg.path) };
      }
      case "close":
        return { result: { closed: this.files.delete(msg.path) } };
      case "addFiles":
        return { result: { added: this.addFiles(msg) } };
      case "readFile": {
        const f = this.files.get(msg.path);
        if (f === undefined) throw new MockError("failed", `no such file: ${msg.path}`);
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
        this.requireMeasurement(msg.measurement);
        return {
          result: {
            plane: `${msg.axis}=${msg.offset}`,
            area: 400,
            perimeter: 80,
            contours: 1,
            bboxMin: [-10, -10, msg.offset],
            bboxMax: [10, 10, msg.offset],
            outline: [[-10, -10, msg.offset, 10, -10, msg.offset, 10, 10, msg.offset, -10, 10, msg.offset]],
          },
        };
      case "between":
        this.requireMeasurement(msg.measurement);
        return {
          result: {
            a: msg.a,
            b: msg.b,
            distance: 4,
            touching: false,
            overlapping: false,
            overlapVolume: 0,
            pointA: [10, 0, 3],
            pointB: [14, 0, 3],
          },
        };
      case "pick":
        this.requireMeasurement(msg.measurement);
        return { result: { point: [0, 0, 10] } };
      case "export":
        return this.export(msg);
      case "lsp":
        return { result: { messages: this.lsp(msg.message) } };
      default:
        throw invalid(`unknown request type '${msg.type}'`);
    }
  }

  docInfo(path) {
    const version = (this.versions.get(path) ?? 0) + 1;
    this.versions.set(path, version);
    const t = this.files.get(path);
    return { path, version, length: typeof t === "string" ? new TextEncoder().encode(t).length : null };
  }

  text(path) {
    const t = this.files.get(path);
    if (typeof t !== "string") throw new MockError("failed", `not open: ${path}`);
    return t;
  }

  requireMeasurement(m) {
    if (m !== this.measurement || !m) throw invalid(`measurement ${m} is not the latest; measure again`);
  }

  addFiles(msg) {
    let n = 0;
    for (const f of msg.files ?? []) {
      this.files.set(f.path, typeof f.data === "string" ? f.data : new Uint8Array(f.data));
      n += 1;
    }
    if (msg.tar) {
      const root = msg.root ?? "/neoscad/libraries";
      for (const f of untar(new Uint8Array(msg.tar))) {
        this.files.set(`${root}/${f.path}`, new TextDecoder().decode(f.bytes));
        n += 1;
      }
    }
    return n;
  }

  run(msg) {
    const text = this.text(msg.path);
    const slow = text.match(/\/\/\s*mock:slow=(\d+)/);
    if (slow) busyWait(Math.min(60000, Number(slow[1])));
    if (/\/\/\s*mock:crash/.test(text)) throw new MockCrash("mock:crash in the text");
    const console = [
      { kind: "info", text: "Mock engine: this build has no wasm core, so results are canned.", location: null },
    ];
    const echo = [];
    const re = /echo\s*\(([^;]*)\)\s*;/g;
    for (let m; (m = re.exec(text)); ) {
      const a = positionAt(text, m.index);
      const b = positionAt(text, m.index + m[0].length);
      const line = `ECHO: ${m[1].trim()}`;
      echo.push(line);
      console.push({
        kind: "echo",
        text: line,
        location: { path: msg.path, startLine: a.line, startCharacter: a.character, endLine: b.line, endCharacter: b.character },
      });
    }
    // An argument naming a variable the text never assigns: OpenSCAD's
    // "unknown variable" warning, with the position a real one carries.
    const known = new Set(["true", "false", "undef", "PI"]);
    for (const m of text.matchAll(/^\s*([A-Za-z_]\w*)\s*=/gm)) known.add(m[1]);
    for (const m of text.matchAll(/\(\s*[A-Za-z_]\w*\s*=\s*([A-Za-z_]\w*)\s*\)/g)) {
      if (known.has(m[1])) continue;
      const at = m.index + m[0].lastIndexOf(m[1]);
      const a = positionAt(text, at);
      const b = positionAt(text, at + m[1].length);
      console.push({
        kind: "warning",
        text: `WARNING: Ignoring unknown variable "${m[1]}" in file ${msg.path.split("/").pop()}, line ${a.line + 1}`,
        location: { path: msg.path, startLine: a.line, startCharacter: a.character, endLine: b.line, endCharacter: b.character },
      });
    }
    if (/\bpart\s*\(/.test(text) && !msg.parts) {
      console.push({ kind: "warning", text: "WARNING: Ignoring unknown module 'part'", location: null });
    }
    // A NeoSCAD extension's module the run did not enable is OpenSCAD's
    // unknown module, as in the core: what lets the mock build's tests see
    // whether a toggle or a link's extensions reached the run.
    for (const [name, modules] of Object.entries(EXTENSION_MODULES)) {
      if ((msg.enable ?? []).includes(name)) continue;
      for (const m of modules) {
        if (new RegExp(`\\b${m}\\s*\\(`).test(text)) {
          console.push({ kind: "warning", text: `WARNING: Ignoring unknown module '${m}'`, location: null });
        }
      }
    }
    const model = mockModel(text, msg.mode);
    this.model = model;
    const b = model.bbox;
    const geometry =
      msg.mode === "preview"
        ? null
        : {
            dimensions: 3,
            bboxMin: b.min,
            bboxMax: b.max,
            area: 2400,
            volume: 8000,
            triangles: model.meshes.length * 12,
            vertices: model.meshes.length * 8,
            manifold: true,
            components: model.meshes.length,
          };
    const ms = 1 + (slow ? Number(slow[1]) : 0);
    const scene = msg.scene === false ? null : packScene(model);
    const vp = text.match(/^\s*\$vpr\s*=\s*\[([^\]]*)\]\s*;/m);
    const timings = { parseMs: 0.1, evaluateMs: 0.4, geometryMs: ms - 0.5, totalMs: ms };
    return {
      result: {
        // The real worker's words come from the core (describe_render);
        // the mock's are near enough for the page's tests.
        summary: msg.mode === "preview" ? `Previewed in ${ms.toFixed(1)} ms.` : `Rendered in ${ms.toFixed(1)} ms.`,
        timingsText: `total ${ms.toFixed(1)} ms`,
        render: {
          exitCode: 0,
          diagnostics: [],
          echo,
          console: console.map((l) => l.text).join("\n"),
          geometry,
          cacheEntries: 0,
          timings,
        },
        console,
        files: [],
        language: [],
        scene,
        fileView: vp ? { vpr: vp[1].split(",").map(Number) } : null,
      },
      transfer: scene ? [scene.faces, scene.edges] : [],
    };
  }

  parts(msg) {
    const text = this.text(msg.path);
    return msg.run?.parts ? [...text.matchAll(/\bpart\s*\(\s*"([^"]+)"/g)].map((m) => m[1]) : [];
  }

  check(msg) {
    const options = msg.options ?? CHECK_DEFAULTS;
    if (!(options.nozzle > 0) || !(options.minWall > 0)) throw invalid("the nozzle and wall must be positive");
    return {
      exitCode: 0,
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
          bboxMin: [4, -2, 0],
          bboxMax: [6, 2, 4],
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
          bboxMin: [-3, -3, 10],
          bboxMax: [3, 3, 10],
          fix: "Chamfer the edge to 45° or add support.",
          value: 50,
          limit: 45,
        },
      ],
      truncated: [],
      minWall: 0.6,
      parts: this.parts(msg),
      text: "check: 1 warning, 1 note",
      summaryJson: "{}",
      diagnostics: [],
      console: "",
    };
  }

  measure(msg) {
    const solid = {
      volume: 8000,
      area: 2400,
      bboxMin: [-10, -10, 0],
      bboxMax: [10, 10, 20],
      centroid: [0, 0, 10],
      triangles: 12,
    };
    this.measurement += 1;
    return {
      exitCode: 0,
      model: solid,
      components: 1,
      manifold: true,
      model2d: null,
      parts: this.parts(msg).map((name) => ({ name, instances: 1, context: null, solid })),
      measurement: this.measurement,
      diagnostics: [],
      console: "",
    };
  }

  export(msg) {
    const model = this.model ?? { meshes: [boxMesh([0, 0, 0], [10, 10, 10])] };
    const step = () => new TextEncoder().encode("ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n");
    const make = { stl: () => stl(model), off: () => off(model), svg, "3mf": () => threeMF(model), step }[msg.format];
    if (!make) throw invalid(`unknown export format: ${msg.format}`);
    // As the core: STEP only with the `exact` extension on the run.
    if (msg.format === "step" && !(msg.run?.enable ?? []).includes("exact")) {
      return {
        result: { exitCode: 1, format: "step", bytes: 0, mime: MIME.step, data: null, geometry: null, diagnostics: [],
          console: "ERROR: STEP export needs NeoSCAD's exact extension (--enable exact).\n",
          timings: { parseMs: 0, evaluateMs: 0, geometryMs: 0, totalMs: 1 }, step: null },
      };
    }
    const bytes = make();
    const data = bytes.buffer;
    return {
      result: {
        exitCode: 0,
        format: msg.format,
        bytes: data.byteLength,
        mime: MIME[msg.format],
        data,
        geometry: null,
        diagnostics: [],
        console: "",
        timings: { parseMs: 0, evaluateMs: 0, geometryMs: 0, totalMs: 1 },
        step: msg.format === "step" ? MOCK_STEP_REPORT : null,
      },
      transfer: [data],
    };
  }

  /// Enough of a language server for the editor's client to initialise
  /// and not wait on its requests: `initialize` gets capabilities, any
  /// other request a null result, notifications nothing.
  lsp(message) {
    let m;
    try {
      m = JSON.parse(message);
    } catch {
      throw invalid("not JSON-RPC");
    }
    if (m.method === "textDocument/didOpen") this.lspOpen.add(m.params.textDocument.uri);
    if (m.id === undefined || m.method === undefined) return [];
    const result =
      m.method === "initialize"
        ? { capabilities: { positionEncoding: "utf-16", textDocumentSync: { openClose: true, change: 2 } } }
        : null;
    return [JSON.stringify({ jsonrpc: "2.0", id: m.id, result })];
  }
}
