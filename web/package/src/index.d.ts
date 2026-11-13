// Types for @neoscad/viewer (hand-written; docs/js-package.md).
//
// Positions: every `line` is 0-based and every `column` a UTF-16 index
// into that line (JavaScript string indexing), end exclusive. A NeoSCAD
// Diagnostic's 1-based byte columns are converted before they get here.

/** The package's version (the workspace version it was built from). */
export const VERSION: string;

/** The worker's resource limits (docs/web-protocol.md, `init`). `null` is unlimited. */
export interface ResourceLimits {
  timeSeconds?: number | null;
  memoryBytes?: number | null;
  fragments?: number | null;
  slices?: number | null;
  list?: number | null;
  string?: number | null;
  rands?: number | null;
  triangles?: number | null;
  depth?: number | null;
}

export interface EngineOptions {
  /** The assets directory (dist/assets). Default: `./assets/` beside dist/index.js. */
  assetsUrl?: string | URL;
  /** Seed of unseeded `rands()`. Default 0, so a model looks the same on every load. */
  seed?: number;
  limits?: ResourceLimits;
  /** Starts the engine's worker yourself (strict CSPs). Must return a new Worker on every call. */
  worker?: () => Worker;
  /** Abandon a model's run that has taken longer than this while a newer run of it waits. Default: never. */
  staleMs?: number;
  /** Fetch fonts.tar.gz the first time a model draws text. Default true. */
  lazyFonts?: boolean;
}

/** A customizer value: number, boolean, string or vector of numbers. */
export type ParameterValue = number | boolean | string | number[];

export interface RunOptions {
  /** Default "preview". */
  mode?: "preview" | "render";
  /** Language extensions (`--enable` names): "sketch", "query", "exact", "fillet", "part", ... */
  enable?: string[] | string;
  /** Customizer values by name, as `-D name=value`. */
  overrides?: Record<string, ParameterValue>;
  /** Files for include/use/import, by path relative to the model ("lib/part.scad"). */
  files?: Record<string, string | ArrayBuffer | ArrayBufferView>;
  /** The render colour scheme the scene's colours are baked in. Default "Cornfield". */
  colorScheme?: string;
  /** Aborting rejects with AbortError; a run in flight is stopped by restarting the worker. */
  signal?: AbortSignal;
  /** The model's file name, which errors and 3MF metadata use. Default "main.scad". */
  name?: string;
}

export interface Location {
  path: string | null;
  line: number;
  column: number;
  endLine: number;
  endColumn: number;
}

export interface ConsoleLine {
  kind: "error" | "warning" | "deprecated" | "echo" | "trace" | "info";
  text: string;
  location: Location | null;
}

export interface Message {
  severity: "error" | "warning";
  /** Without OpenSCAD's "ERROR: " prefix. */
  message: string;
  /** OpenSCAD's line, word for word. */
  text: string;
  /** Relative to the model's directory, or a library's absolute in-memory path. */
  path: string | null;
  line: number | null;
  column: number | null;
  endLine: number | null;
  endColumn: number | null;
  code: string | null;
  hints: string[];
}

/** The packed scene the viewer draws (docs/web-protocol.md, PackedScene). */
export interface PackedScene {
  faces: ArrayBuffer;
  edges: ArrayBuffer;
  meta: string;
}

export interface FileView {
  vpt?: [number, number, number];
  vpr?: [number, number, number];
  vpd?: number;
  vpf?: number;
}

export interface RunResult {
  ok: boolean;
  exitCode: number;
  summary: string;
  scene: PackedScene | null;
  console: ConsoleLine[];
  errors: Message[];
  warnings: Message[];
  bbox: { min: [number, number, number]; max: [number, number, number] } | null;
  /** Render mode only. */
  geometry: Record<string, unknown> | null;
  /** The `$vp*` the file assigned. */
  fileView: FileView | null;
  timings: { parseMs: number; evaluateMs: number; geometryMs: number; totalMs: number } | null;
  superseded?: undefined;
}

/** A coalesced run replaced by a newer one of the same model before it ran. */
export interface Superseded {
  superseded: true;
}

export interface ExportOptions extends RunOptions {
  /** 3MF/PDF metadata date. Default "1970-01-01T00:00:00Z", for identical bytes on every visit. */
  creationDate?: string;
  /** Format options, e.g. `{threemfColorMode: "model"}`. */
  options?: Record<string, unknown>;
}

export interface ExportResult {
  data: Uint8Array;
  /** False for a file written with errors (failed fillets: those edges are sharp). */
  ok: boolean;
  exitCode: number;
  failure: string | null;
  mime: string | null;
  format: string | null;
  messages: Message[];
}

export type ExportFormat = "binstl" | "stl" | "3mf" | "off" | "obj" | "wrl" | "pov" | "svg" | "dxf" | "pdf" | "step";

export interface ParameterGroup {
  name: string;
  parameters: { name: string; description: string; control: { kind: string; [k: string]: unknown }; defaultValue: ParameterValue }[];
}

export interface EngineInfo {
  version: string;
  limits: ResourceLimits;
  libraryDirs: string[];
}

export class Model {
  run(source: string, options?: RunOptions): Promise<RunResult | Superseded>;
  export(source: string, format: ExportFormat, options?: ExportOptions): Promise<ExportResult>;
  parameters(source: string, options?: RunOptions): Promise<ParameterGroup[]>;
  dispose(): void;
}

export class Engine {
  /** Resolves once the worker answers; rejects with EngineStartError. */
  readonly ready: Promise<EngineInfo>;
  /** The engine's version (from `init`), once ready. */
  readonly version: string | null;
  readonly assets: URL;
  /** A document the engine keeps for repeated runs, coalesced latest-wins. */
  model(options?: { name?: string; camera?: () => Camera | null }): Model;
  /** One run; never coalesced with another call. */
  run(source: string, options?: RunOptions): Promise<RunResult>;
  export(source: string, format: ExportFormat, options?: ExportOptions): Promise<ExportResult>;
  parameters(source: string, options?: RunOptions): Promise<ParameterGroup[]>;
  dispose(): void;
}

export function createEngine(options?: EngineOptions): Engine;
/** The page's engine for an assets directory, shared by every element and `render()`. */
export function sharedEngine(options?: EngineOptions): Engine;

export interface ViewSettings {
  axes?: boolean;
  scales?: boolean;
  grid?: boolean;
  edges?: boolean;
  crosshairs?: boolean;
  orthographic?: boolean;
  lighting?: string;
  scheme?: string;
}

export const DEFAULT_VIEW: Required<ViewSettings>;

export interface Camera {
  vpt: [number, number, number];
  vpr: [number, number, number];
  vpd: number;
  vpf: number;
}

export interface ViewerOptions {
  assetsUrl?: string | URL;
  /** Default "auto": WebGPU, then WebGL2, then a simplified canvas2d view. */
  backend?: "auto" | "webgpu" | "webgl" | "canvas2d";
  settings?: ViewSettings;
}

export type Preset = "top" | "bottom" | "left" | "right" | "front" | "back" | "diagonal";

export class Viewer {
  readonly kind: "webgpu" | "webgl" | "canvas2d";
  /** Shown over the view when it fell back to canvas2d, else null. */
  readonly notice: string | null;
  readonly canvas: HTMLCanvasElement;
  readonly schemes: string[];
  setScene(scene: PackedScene | null): void;
  setSettings(settings: ViewSettings): void;
  preset(name: Preset): void;
  viewAll(): void;
  resetView(): void;
  camera(): Camera;
  setFileView(view: FileView | null): void;
  resize(): void;
  /** The view drawn at width x height, as a PNG (or `type`) Blob. */
  image(width: number, height: number, options?: { type?: string }): Promise<Blob>;
  dispose(): void;
}

/** A viewer on a canvas, or on a canvas made inside a container element. */
export function createViewer(target: HTMLCanvasElement | HTMLElement, options?: ViewerOptions): Promise<Viewer>;

export interface RenderOptions extends EngineOptions, ViewerOptions, Omit<RunOptions, "signal"> {
  /** An engine to use instead of the page's shared one. */
  engine?: Engine;
  onResult?: (result: RunResult) => void;
}

export class View {
  readonly engine: Engine;
  readonly viewer: Viewer | null;
  /** The last result shown. */
  readonly result: RunResult | null;
  readonly source: string | null;
  update(source?: string, options?: RunOptions): Promise<RunResult | Superseded>;
  export(format: ExportFormat, options?: ExportOptions): Promise<ExportResult>;
  dispose(): void;
}

/** Engine and viewer in one: resolves after the first run is shown (a failing model too). */
export function render(target: HTMLCanvasElement | HTMLElement, source: string, options?: RenderOptions): Promise<View>;

export class EngineStartError extends Error {}
export class AbortError extends Error {}
export class ExportError extends Error {
  readonly failure: string | null;
  readonly exitCode: number;
}

/** Whether two file views differ (move the view only when they do). */
export function fileViewChanged(a: FileView | null, b: FileView | null): boolean;
/** Throws a TypeError unless `path` is relative and stays inside the model's directory. */
export function checkRelativePath(path: string): string;
