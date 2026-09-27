// OpenSCAD language support for CodeMirror: the Lezer parser
// (openscad.grammar) with highlighting tags, folding, indentation, comment
// tokens and bracket closing.

import {
  LRLanguage,
  LanguageSupport,
  continuedIndent,
  delimitedIndent,
  foldInside,
  foldNodeProp,
  indentNodeProp,
  indentUnit,
} from "@codemirror/language";
import { styleTags, tags as t } from "@lezer/highlight";
import { parser } from "./parser.js";

export { parser };

// A statement that continues past its first line (`translate(v)` then the
// child on the next line, a long assignment) is indented one unit, except
// for a line that opens a block: `{` goes back to the statement's column.
const statementIndent = continuedIndent({ except: /^\s*\{/ });

export const openscadLanguage = LRLanguage.define({
  name: "openscad",
  parser: parser.configure({
    props: [
      styleTags({
        "module function": t.definitionKeyword,
        "if else for each let": t.controlKeyword,
        "assert echo": t.keyword,
        "include use": t.moduleKeyword,
        "true false": t.bool,
        undef: t.null,
        Number: t.number,
        // "/..." styles the node's child tokens too (a string's closing
        // quote, an operator's symbol): without it a child node is drawn
        // unstyled over its own range, even when it spans all of its parent.
        "String/...": t.string,
        Escape: t.escape,
        IncludePath: t.special(t.string),
        LineComment: t.lineComment,
        BlockComment: t.blockComment,
        SpecialVariable: t.special(t.variableName),
        VariableName: t.variableName,
        "VariableDefinition ParamName": t.definition(t.variableName),
        DefName: t.function(t.definition(t.variableName)),
        ModuleName: t.function(t.variableName),
        "CallExpression/VariableName": t.function(t.variableName),
        ArgName: t.attributeName,
        PropertyName: t.propertyName,
        "Modifier/...": t.modifier,
        "ArithOp/...": t.arithmeticOperator,
        "LogicOp/...": t.logicOperator,
        "BitOp/...": t.bitwiseOperator,
        "CompareOp/...": t.compareOperator,
        "=": t.definitionOperator,
        "( )": t.paren,
        "[ ]": t.squareBracket,
        "{ }": t.brace,
        ". , ;": t.separator,
      }),
      indentNodeProp.add({
        Block: delimitedIndent({ closing: "}" }),
        "ArgList ParamList ParenthesizedCondition ParenthesizedExpression ForHeader":
          delimitedIndent({ closing: ")", align: true }),
        "Vector Range": delimitedIndent({ closing: "]", align: true }),
        "ModuleCall IfStatement ModifiedStatement ModuleDefinition FunctionDefinition Assignment":
          statementIndent,
      }),
      foldNodeProp.add({
        "Block ArgList ParamList Vector": foldInside,
        BlockComment(node) {
          return { from: node.from + 2, to: node.to - 2 };
        },
      }),
    ],
  }),
  languageData: {
    commentTokens: { line: "//", block: { open: "/*", close: "*/" } },
    closeBrackets: { brackets: ["(", "[", "{", '"'] },
    // Re-indent a line as soon as it is only a closing bracket.
    indentOnInput: /^\s*[\}\]\)]$/,
  },
});

/// The language, indenting by four spaces: the style of OpenSCAD's own
/// examples, BOSL2, and `neoscad fmt` (docs/cli-json.md, "Style").
/// CodeMirror's default unit is two.
export function openscad() {
  return new LanguageSupport(openscadLanguage, [indentUnit.of("    ")]);
}
