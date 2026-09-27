// The NeoSCAD editor: CodeMirror 6 with OpenSCAD support, in the macOS
// app's web view. The app is the other end of every message; the protocol
// is described in apple/App/Editor/EditorController.swift.
//
// Who owns the text: CodeMirror, while editing. It holds the selection and
// the undo history, and every change goes to the app as it happens, in
// order, where the app keeps an exact copy (it needs the text before each
// change to turn UTF-16 offsets into the core's UTF-8 ones, and NSDocument
// saves synchronously). The app replaces the whole text only when the
// document is read (open, revert), through `load`.

import {
  closeBrackets,
  closeBracketsKeymap,
  completionStatus,
  currentCompletions,
  startCompletion,
} from "@codemirror/autocomplete";
import {
  defaultKeymap,
  history,
  historyKeymap,
  indentWithTab,
  redo,
  redoDepth,
  selectAll,
  undo,
  undoDepth,
} from "@codemirror/commands";
import {
  bracketMatching,
  foldGutter,
  foldKeymap,
  indentOnInput,
} from "@codemirror/language";
import { forEachDiagnostic, lintGutter, lintKeymap } from "@codemirror/lint";
import { LSPPlugin, formatDocument, showSignatureHelp } from "@codemirror/lsp-client";
import {
  highlightSelectionMatches,
  openSearchPanel,
  search,
  searchKeymap,
} from "@codemirror/search";
import { Compartment, EditorState } from "@codemirror/state";
import {
  EditorView,
  crosshairCursor,
  drawSelection,
  dropCursor,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
} from "@codemirror/view";
import { Versions, changesToEdits, editKind, lspTransport } from "./bridge.js";
import { goToDefinition, languageClient } from "./language.js";
import { builtinHighlighter } from "./lang/builtins.js";
import { openscad } from "./lang/openscad.js";
import { fontTheme, themes } from "./theme.js";

// --- The channel to the app -----------------------------------------------

const handler = window.webkit?.messageHandlers?.editor;

/// Send a message to the app; its reply, or null outside the app.
function post(message) {
  if (!handler) return Promise.resolve(null);
  return handler.postMessage(message).catch((e) => {
    console.error("editor bridge:", e);
    return null;
  });
}

window.addEventListener("error", (e) => {
  post({ type: "log", level: "error", message: `${e.message} (${e.filename}:${e.lineno})` });
});
window.addEventListener("unhandledrejection", (e) => {
  post({ type: "log", level: "error", message: `unhandled rejection: ${e.reason}` });
});

// --- State ----------------------------------------------------------------

const versions = new Versions();
const lsp = lspTransport(post);

/// A location in another file, for the app to open.
function openLocation(uri, line, character) {
  post({ type: "open", uri, line, character });
}

// The language server's client: connected at once (the app answers from
// the start), attached to the document once the app says which file it is.
const client = languageClient(lsp, openLocation);
const lspSlot = new Compartment();
const readOnlySlot = new Compartment();
/// The document's `file://` URI and whether it is read-only (a library
/// file), as the last load gave them.
let documentURI = null;
let readOnly = false;

const themeSlot = new Compartment();
const fontSlot = new Compartment();
const darkQuery = window.matchMedia("(prefers-color-scheme: dark)");
let fontSize = 12;

const currentTheme = () => (darkQuery.matches ? themes.dark : themes.light);

// The page's Content-Security-Policy admits only styles with this nonce
// (apple/App/Editor/EditorSchemeHandler.swift writes both).
const nonce = document.querySelector('meta[name="neoscad-nonce"]')?.content ?? "";

/// Sends each change to the app as it happens.
const changeReporter = EditorView.updateListener.of((update) => {
  for (const tr of update.transactions) {
    // A load replaces the state without a transaction, so every change
    // seen here is the user's (or a fix's) and goes to the app.
    if (!tr.docChanged) continue;
    const base = versions.version;
    const version = versions.push();
    post({
      type: "changes",
      base,
      version,
      edits: changesToEdits(tr.changes),
      kind: editKind(tr),
      length: tr.newDoc.length,
      undoDepth: undoDepth(tr.state),
      redoDepth: redoDepth(tr.state),
    });
  }
});

/// Keys of the app's menu that carry no modifier: F5 (Preview) and F6
/// (Render). The editor forwards them itself rather than relying on WebKit
/// to hand an unmodified key the page did not use back to the menu. A key
/// is acted on once either way: if the menu takes it first the page never
/// sees it, and a key taken here is marked handled, which stops WebKit
/// from passing it on.
const appKeys = keymap.of([
  { key: "F5", run: () => (post({ type: "command", name: "preview" }), true) },
  { key: "F6", run: () => (post({ type: "command", name: "render" }), true) },
]);

function extensions() {
  return [
    lspSlot.of(documentURI ? client.plugin(documentURI, "openscad") : []),
    readOnlySlot.of(readOnly ? EditorState.readOnly.of(true) : []),
    // Only "\n" separates lines, so a "\r\n" or lone "\r" in a file stays
    // in the text as it is. CodeMirror's default splits on all three and
    // joins with "\n", which would change the file's bytes on save and make
    // every offset after the first "\r\n" disagree with the app's copy.
    EditorState.lineSeparator.of("\n"),
    EditorView.cspNonce.of(nonce),
    lineNumbers(),
    highlightActiveLineGutter(),
    highlightSpecialChars(),
    history(),
    foldGutter(),
    drawSelection(),
    dropCursor(),
    EditorState.allowMultipleSelections.of(true),
    indentOnInput(),
    bracketMatching(),
    closeBrackets(),
    rectangularSelection(),
    crosshairCursor(),
    highlightActiveLine(),
    highlightSelectionMatches(),
    search({ top: true }),
    lintGutter(),
    openscad(),
    builtinHighlighter,
    themeSlot.of(currentTheme()),
    fontSlot.of(fontTheme(fontSize)),
    appKeys,
    keymap.of([
      ...closeBracketsKeymap,
      ...defaultKeymap,
      ...searchKeymap,
      ...historyKeymap,
      ...foldKeymap,
      ...lintKeymap,
      indentWithTab,
    ]),
    EditorView.contentAttributes.of({
      "aria-label": "OpenSCAD source",
      autocapitalize: "off",
      autocorrect: "off",
      spellcheck: "false",
    }),
    changeReporter,
  ];
}

const view = new EditorView({
  parent: document.getElementById("editor"),
  state: EditorState.create({ doc: "", extensions: extensions() }),
});

darkQuery.addEventListener("change", () => {
  view.dispatch({ effects: themeSlot.reconfigure(currentTheme()) });
});

function historyState() {
  return {
    version: versions.version,
    undoDepth: undoDepth(view.state),
    redoDepth: redoDepth(view.state),
  };
}

/// Poll `get` until it returns something other than null or undefined,
/// for at most `ms` milliseconds (the tests' helpers wait for the language
/// server's answers this way).
function until(get, ms = 5000) {
  return new Promise((resolve) => {
    const t0 = performance.now();
    const tick = () => {
      const v = get();
      if (v !== null && v !== undefined) resolve(v);
      else if (performance.now() - t0 > ms) resolve(null);
      else setTimeout(tick, 10);
    };
    tick();
  });
}

// --- What the app calls (callAsyncJavaScript) -----------------------------

window.NeoSCADEditor = {
  /// Replace the document (the file was read): a new state, so the undo
  /// history starts over, as it does for a reverted NSDocument. `uri` is
  /// the file's (`file://`, for the language server; null for none) and
  /// `readOnly` marks a library file shown for reading.
  load(text, uri = null, readOnlyFile = false) {
    documentURI = uri;
    readOnly = readOnlyFile;
    view.setState(EditorState.create({ doc: text, extensions: extensions() }));
    versions.reset();
    return historyState();
  },

  /// The whole text, for the app to resynchronise its copy.
  text() {
    return { version: versions.version, text: view.state.doc.toString() };
  },

  /// The document was saved under another name: the language server
  /// sees the file close and the new one open.
  setURI(uri) {
    documentURI = uri;
    view.dispatch({
      effects: lspSlot.reconfigure(uri ? client.plugin(uri, "openscad") : []),
    });
    return true;
  },

  /// Put the cursor at a 0-based line and UTF-16 column (a definition the
  /// app opened this file for), scrolled into view.
  reveal(line, character) {
    const doc = view.state.doc;
    const l = doc.line(Math.min(Math.max(line + 1, 1), doc.lines));
    const at = Math.min(l.from + character, l.to);
    view.dispatch({ selection: { anchor: at }, scrollIntoView: true });
    return at;
  },

  setFontSize(size) {
    fontSize = size;
    view.dispatch({ effects: fontSlot.reconfigure(fontTheme(size)) });
    return true;
  },

  // Edit menu commands, sent by the app when a menu item is chosen with
  // the mouse (keys reach CodeMirror's keymap directly).
  undo: () => undo(view),
  redo: () => redo(view),
  selectAll: () => selectAll(view),
  openSearch: () => openSearchPanel(view),
  focus() {
    view.focus();
    return view.hasFocus;
  },

  /// A message from the language server to the LSP client.
  lspReceive(message) {
    lsp.receive(message);
    return true;
  },

  // --- For the app's tests and measurements ---

  /// Replace `from..to` with `insert` as typing would (a user event, in
  /// the undo history): what the app's tests use to edit "by hand".
  edit(from, to, insert) {
    view.dispatch({
      changes: { from, to, insert },
      selection: { anchor: from + insert.length },
      userEvent: "input.type",
      scrollIntoView: true,
    });
    return historyState();
  },

  /// Place the cursor (UTF-16 offset).
  select(anchor, head = anchor) {
    view.dispatch({ selection: { anchor, head }, scrollIntoView: true });
    return true;
  },

  // The language features as a person would trigger them, answered by
  // the server (for the app's tests): each resolves to what was shown,
  // or null when nothing came.

  /// Completion at `pos`: the labels offered.
  async complete(pos) {
    view.dispatch({ selection: { anchor: pos } });
    startCompletion(view);
    return until(() =>
      completionStatus(view.state) === "active" && currentCompletions(view.state).length
        ? currentCompletions(view.state).map((c) => c.label)
        : null,
    );
  },

  /// The hover tooltip's text at `pos`, as the pointer resting there shows it.
  async hover(pos) {
    const c = view.coordsAtPos(pos);
    if (!c) return null;
    const x = c.left + 1;
    const y = (c.top + c.bottom) / 2;
    for (const type of ["mouseenter", "mousemove"]) {
      view.contentDOM.dispatchEvent(
        new MouseEvent(type, { clientX: x, clientY: y, bubbles: true }),
      );
    }
    return until(() => document.querySelector(".cm-tooltip-hover")?.textContent || null);
  },

  /// The signature help shown at `pos`.
  async signature(pos) {
    view.dispatch({ selection: { anchor: pos } });
    showSignatureHelp(view);
    return until(() => document.querySelector(".cm-lsp-signature-tooltip")?.textContent || null);
  },

  /// A key pressed in the editor as WebKit would deliver it to the page
  /// (`init` as for a KeyboardEvent): whether a binding took it.
  key(init) {
    view.focus();
    const e = new KeyboardEvent("keydown", { ...init, bubbles: true, cancelable: true });
    // The constructor ignores the legacy `keyCode`, which CodeMirror reads
    // to find the key under an Option combination (⌥⇧F types "Ï").
    if (init.keyCode) Object.defineProperty(e, "keyCode", { get: () => init.keyCode });
    return !view.contentDOM.dispatchEvent(e);
  },

  /// Go to the definition of the name at `pos` (F12).
  definition(pos) {
    view.dispatch({ selection: { anchor: pos } });
    return goToDefinition(view, openLocation);
  },

  /// Format the document (Shift-Alt-F): the text once it changed.
  async format() {
    const before = view.state.doc.toString();
    formatDocument(view);
    return until(() => (view.state.doc.toString() !== before ? view.state.doc.toString() : null));
  },

  /// Time `count` completion requests at `completeAt` and hover requests
  /// at `hoverAt` (after one of each to warm up), each from the page's
  /// client through the app to the server and back: milliseconds, p50 and
  /// p95 (WebKit's clock has 1 ms resolution).
  async benchmarkLanguage(completeAt, hoverAt, count) {
    const plugin = LSPPlugin.get(view);
    if (!plugin) return null;
    client.sync();
    const doc = { uri: plugin.uri };
    const params = { textDocument: doc, position: plugin.toPosition(hoverAt) };
    const completion = {
      textDocument: doc,
      position: plugin.toPosition(completeAt),
      context: { triggerKind: 1 },
    };
    await client.request("textDocument/completion", completion);
    await client.request("textDocument/hover", params);
    const c = [];
    const h = [];
    for (let i = 0; i < count; i++) {
      let t0 = performance.now();
      await client.request("textDocument/completion", completion);
      c.push(performance.now() - t0);
      t0 = performance.now();
      await client.request("textDocument/hover", params);
      h.push(performance.now() - t0);
    }
    const at = (v, q) => v.sort((a, b) => a - b)[Math.min(v.length - 1, Math.floor(q * v.length))];
    return {
      completion: { p50: at(c, 0.5), p95: at(c, 0.95) },
      hover: { p50: at(h, 0.5), p95: at(h, 0.95) },
    };
  },

  /// Resolves once the language server answered `initialize`.
  async lspReady() {
    await client.initializing;
    return client.serverCapabilities !== null;
  },

  state() {
    let diagnostics = 0;
    const fixes = [];
    forEachDiagnostic(view.state, (d) => {
      diagnostics++;
      for (const a of d.actions ?? []) fixes.push(a.name);
    });
    const sel = view.state.selection.main;
    return {
      ...historyState(),
      length: view.state.doc.length,
      lines: view.state.doc.lines,
      focused: view.hasFocus,
      selection: [sel.anchor, sel.head],
      diagnostics,
      fixes,
      dark: darkQuery.matches,
      fontSize,
      uri: documentURI,
      readOnly,
      lsp: client.serverCapabilities !== null,
    };
  },

  /// Time `count` single-character insertions at the cursor, each
  /// dispatched and laid out (the synchronous work of a keystroke in
  /// CodeMirror). They stay in the text and go to the app like typing.
  /// Milliseconds per keystroke: p50, p95, max (WebKit's clock has 1 ms
  /// resolution).
  benchmarkTyping(count) {
    const times = [];
    for (let i = 0; i < count; i++) {
      const pos = view.state.selection.main.head;
      const t0 = performance.now();
      view.dispatch({
        changes: { from: pos, insert: "x" },
        selection: { anchor: pos + 1 },
        userEvent: "input.type",
        scrollIntoView: true,
      });
      // Force the layout the next frame would do.
      view.coordsAtPos(pos + 1);
      times.push(performance.now() - t0);
    }
    times.sort((a, b) => a - b);
    const at = (q) => times[Math.min(times.length - 1, Math.floor(q * times.length))];
    return { p50: at(0.5), p95: at(0.95), max: times[times.length - 1] };
  },
};

post({ type: "ready" });
