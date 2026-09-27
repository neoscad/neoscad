// The external tokenizer of openscad.grammar: numbers and digit-led names,
// with lexer.l's rules and flex's longest-match choice between them.

import { ExternalTokenizer } from "@lezer/lr";
import { Number, digitIdentifier } from "./parser.terms.js";

const isDigit = (c) => c >= 48 && c <= 57;
const isHex = (c) => isDigit(c) || (c >= 65 && c <= 70) || (c >= 97 && c <= 102);

/// lexer.l's IDREST, widened as its UNICODEID is: ASCII letters, digits and
/// `_`, and any non-ASCII UTF-16 unit but U+00A0 and U+FEFF (whitespace).
export function isNameChar(c) {
  return (
    isDigit(c) ||
    (c >= 65 && c <= 90) ||
    (c >= 97 && c <= 122) ||
    c === 95 ||
    (c >= 0x80 && c !== 0xa0 && c !== 0xfeff)
  );
}

/// The length of the longest number at the start of `peek` (a function from
/// an offset to a UTF-16 unit, -1 past the end), or 0. lexer.l's rules:
/// 0x{H}+, {D}+{E}, {D}*\.{D}+{E}?, {D}+\.{D}*{E}?, {D}+.
export function numberLength(peek) {
  let i = 0;
  if (peek(0) === 48 && peek(1) === 120 && isHex(peek(2))) {
    i = 2;
    while (isHex(peek(i))) i++;
    return i;
  }
  while (isDigit(peek(i))) i++;
  const intDigits = i;
  if (peek(i) === 46) {
    let j = i + 1;
    while (isDigit(peek(j))) j++;
    // `1.` is a number; `.` alone is not.
    if (intDigits > 0 || j > i + 1) i = j;
  }
  if (i === 0) return 0;
  // The exponent, only when digits follow it.
  if (peek(i) === 101 || peek(i) === 69) {
    let j = i + 1;
    if (peek(j) === 43 || peek(j) === 45) j++;
    if (isDigit(peek(j))) {
      while (isDigit(peek(j))) j++;
      i = j;
    }
  }
  return i;
}

/// The length of lexer.l's {D}{IDREST}* at the start of `peek`, or 0.
export function digitNameLength(peek) {
  if (!isDigit(peek(0))) return 0;
  let i = 1;
  while (isNameChar(peek(i))) i++;
  return i;
}

export const numbers = new ExternalTokenizer((input) => {
  const c = input.next;
  if (!isDigit(c) && !(c === 46 && isDigit(input.peek(1)))) return;
  const peek = (i) => input.peek(i);
  const number = numberLength(peek);
  const name = digitNameLength(peek);
  // flex: the longest match; on a tie, the rule listed first (the number).
  if (name > number) input.acceptToken(digitIdentifier, name);
  else if (number > 0) input.acceptToken(Number, number);
});
