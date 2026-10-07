// The editor's colours: OpenSCAD's own editor schemes, paired with the 3D
// view's as OpenSCAD pairs them. The viewport draws Cornfield in a light
// appearance and Tomorrow Night in a dark one
// (apple/App/Viewport/ViewportController.swift). OpenSCAD's defaults put
// Cornfield beside the "For Light Background" editor scheme
// (.reference/openscad/src/gui/Preferences.cc:177 and :376), and it has a
// Tomorrow Night editor scheme to match the render scheme of that name.
// The colours are those files' (.reference/openscad/color-schemes/editor/
// light-background.json and tomorrow-night.json), with CSS names written
// out.
//
// One deliberate difference: CodeMirror draws the selection behind the
// text and keeps the text's colours, while OpenSCAD's schemes pair a
// selection background with a selection foreground. Taken as they are,
// Tomorrow Night's (#c5c8c6 behind #c5c8c6 text) would hide the selected
// text, and the light scheme's (#4a90d9) would put dark text on dark blue.
// So the dark scheme uses its "selection-foreground" (#373b41, Tomorrow
// Night's usual selection colour) as the background, and the light scheme
// a light tint of its blue.

import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { EditorView } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";

const light = {
  dark: false,
  paper: "#ffffff",
  text: "#272822",
  caret: "#000000",
  caretLine: "#f8f8f8",
  selection: "#c5dcf4",
  marginBackground: "#f8f8f8",
  marginForeground: "#808080",
  matchedBraceBackground: "#c7f6cb",
  matchedBraceForeground: "#0000ff",
  unmatchedBraceBackground: "#ffcdcc",
  unmatchedBraceForeground: "#0000ff",
  keyword: "#008000",
  transformation: "#00008b",
  boolean: "#00008b",
  function: "#008000",
  model: "#00008b",
  specialVariable: "#008000",
  comment: "#008b8b",
  number: "#8b0000",
  string: "#8b008b",
  operator: "#0000ff",
};

const tomorrowNight = {
  dark: true,
  paper: "#1d1f21",
  text: "#c5c8c6",
  caret: "#ffffff",
  caretLine: "#282a2e",
  selection: "#373b41",
  marginBackground: "#1d1f21",
  marginForeground: "#969896",
  matchedBraceBackground: "#50545c",
  matchedBraceForeground: "#e2e6e3",
  unmatchedBraceBackground: "#8a1111",
  unmatchedBraceForeground: "#e2e6e3",
  keyword: "#de935f",
  transformation: "#81a2be",
  boolean: "#81a2be",
  function: "#b294bb",
  model: "#81a2be",
  specialVariable: "#de935f",
  comment: "#969896",
  number: "#cc6666",
  string: "#b5bd68",
  operator: "#8abeb7",
};

export const schemes = { light, dark: tomorrowNight };

function editorTheme(s) {
  return EditorView.theme(
    {
      "&": { color: s.text, backgroundColor: s.paper, height: "100%" },
      ".cm-content": { caretColor: s.caret },
      ".cm-cursor, .cm-dropCursor": { borderLeftColor: s.caret, borderLeftWidth: "2px" },
      "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection":
        { backgroundColor: s.selection },
      ".cm-activeLine": { backgroundColor: s.caretLine },
      ".cm-gutters": {
        backgroundColor: s.marginBackground,
        color: s.marginForeground,
        borderRight: "none",
      },
      ".cm-activeLineGutter": { backgroundColor: s.caretLine },
      "&.cm-focused .cm-matchingBracket": {
        backgroundColor: s.matchedBraceBackground,
        color: s.matchedBraceForeground,
      },
      "&.cm-focused .cm-nonmatchingBracket": {
        backgroundColor: s.unmatchedBraceBackground,
        color: s.unmatchedBraceForeground,
      },
      ".cm-scad-keyword": { color: s.keyword },
      ".cm-scad-transformation": { color: s.transformation },
      ".cm-scad-boolean": { color: s.boolean },
      ".cm-scad-function": { color: s.function },
      ".cm-scad-model": { color: s.model },
      // The sketch vocabulary inside a sketch body: entities and
      // constraints are geometry, coloured as the primitives are.
      ".cm-scad-sketch": { color: s.model },
      ".cm-scad-value": { color: s.number },
      ".cm-panels": { backgroundColor: s.marginBackground, color: s.text },
      ".cm-tooltip": { backgroundColor: s.marginBackground, color: s.text },
    },
    { dark: s.dark },
  );
}

function highlightStyle(s) {
  return HighlightStyle.define([
    {
      tag: [t.keyword, t.controlKeyword, t.definitionKeyword, t.moduleKeyword],
      color: s.keyword,
    },
    { tag: [t.number, t.bool, t.null], color: s.number },
    { tag: [t.string, t.special(t.string)], color: s.string },
    { tag: t.escape, color: s.string, fontWeight: "bold" },
    { tag: t.comment, color: s.comment },
    { tag: t.special(t.variableName), color: s.specialVariable },
    { tag: [t.operator, t.modifier, t.definitionOperator], color: s.operator },
    // Names stay the text colour, as in OpenSCAD; builtins are coloured by
    // builtins.js, whose marks would be hidden by a colour set here.
  ]);
}

/// Each scheme as editor extensions: the chrome and the highlight style.
export const themes = {
  light: [editorTheme(light), syntaxHighlighting(highlightStyle(light))],
  dark: [editorTheme(tomorrowNight), syntaxHighlighting(highlightStyle(tomorrowNight))],
};

/// The font: SF Mono (the system's monospaced face) at `size` pixels.
export function fontTheme(size) {
  return EditorView.theme({
    ".cm-scroller": {
      fontFamily: 'ui-monospace, "SF Mono", Menlo, monospace',
      fontSize: `${size}px`,
      lineHeight: "1.4",
    },
  });
}
