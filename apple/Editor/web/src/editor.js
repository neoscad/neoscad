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

import { closeBrackets, closeBracketsKeymap } from "@codemirror/autocomplete";
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
import { forEachDiagnostic, lintGutter, lintKeymap, setDiagnostics } from "@codemirror/lint";
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
import {
  VersionHistory,
  changesToEdits,
  editKind,
  lspTransport,
  mapDiagnostics,
} from "./bridge.js";
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

const versions = new VersionHistory();
const lsp = lspTransport(post);

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
    const version = versions.push(tr.changes);
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

function applyFix(v, from, to, insert) {
  v.dispatch({ changes: { from, to, insert }, userEvent: "input.fix" });
}

// --- What the app calls (callAsyncJavaScript) -----------------------------

window.NeoSCADEditor = {
  /// Replace the document (the file was read): a new state, so the undo
  /// history starts over, as it does for a reverted NSDocument.
  load(text) {
    view.setState(EditorState.create({ doc: text, extensions: extensions() }));
    versions.reset();
    return historyState();
  },

  /// The whole text, for the app to resynchronise its copy.
  text() {
    return { version: versions.version, text: view.state.doc.toString() };
  },

  /// Lint markers: `list` holds UTF-16 ranges in the text of `version`,
  /// mapped here through the edits made since. How many are shown.
  setDiagnostics(version, list) {
    const diagnostics = mapDiagnostics(versions, version, list, applyFix);
    view.dispatch(setDiagnostics(view.state, diagnostics));
    return diagnostics.length;
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

  /// A message from the language server (8e) to the LSP client.
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

  state() {
    let diagnostics = 0;
    forEachDiagnostic(view.state, () => diagnostics++);
    const sel = view.state.selection.main;
    return {
      ...historyState(),
      length: view.state.doc.length,
      lines: view.state.doc.lines,
      focused: view.hasFocus,
      selection: [sel.anchor, sel.head],
      diagnostics,
      dark: darkQuery.matches,
      fontSize,
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
