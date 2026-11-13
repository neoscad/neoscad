// Types for "@neoscad/viewer/element": importing it defines <neoscad-viewer>.

import type { Engine, ExportFormat, ExportOptions, ExportResult, Model, RunResult, Viewer } from "./index.js";

export const OFFSCREEN_MS: number;

export interface NeoSCADErrorDetail {
  message: string;
  /** The exception, when the engine or a fetch failed. */
  error: Error | null;
  /** The run's result, when the model failed. */
  result: RunResult | null;
}

export class NeoSCADViewer extends HTMLElement {
  /** The model's text: set it to replace `src` and the inline script. */
  source: string | null;
  readonly result: RunResult | null;
  readonly viewer: Viewer | null;
  readonly engine: Engine | null;
  readonly model: Model | null;
  update(): Promise<RunResult | null>;
  export(format: ExportFormat, options?: ExportOptions): Promise<ExportResult>;
  image(width: number, height: number, options?: { type?: string }): Promise<Blob | null>;
}

declare global {
  interface HTMLElementTagNameMap {
    "neoscad-viewer": NeoSCADViewer;
  }
  interface HTMLElementEventMap {
    "neoscad-result": CustomEvent<RunResult>;
    "neoscad-error": CustomEvent<NeoSCADErrorDetail>;
    "neoscad-viewer": CustomEvent<{ kind: string }>;
  }
}
