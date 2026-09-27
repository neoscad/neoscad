// Between the editor's offsets and the core's.
//
// CodeMirror, like every JavaScript editor, counts UTF-16 code units; the
// core (`TextEdit`, diagnostic spans) counts UTF-8 bytes. The two agree on
// ASCII and nowhere else: "é" is one unit and two bytes, "漢" one and
// three, "😀" two and four. Every edit from the editor and every
// diagnostic to it crosses here.

import Foundation

/// A replacement in an editor's UTF-16 offsets: `from..<to` becomes
/// `insert`.
public struct UTF16Edit: Sendable, Equatable {
    public var from: Int
    public var to: Int
    public var insert: String

    public init(from: Int, to: Int, insert: String) {
        self.from = from
        self.to = to
        self.insert = insert
    }
}

public enum TextOffsetError: Error, Equatable {
    /// An offset is negative, past the end, or `to` is before `from`.
    case outOfRange(Int)
    /// An offset falls between the two halves of a surrogate pair: the
    /// editor's text and this one disagree.
    case splitsCharacter(Int)
}

public enum TextOffsets {
    /// Applies `edits` to `text`, each to the text the previous one left
    /// (as `TextEdit`s apply in the core), and returns them in UTF-8 byte
    /// offsets for `Core.edit`.
    ///
    /// An error means the editor's text and `text` no longer agree; `text`
    /// then holds the edits before the failing one, and the caller must
    /// replace it with the editor's whole text.
    public static func apply(_ edits: [UTF16Edit], to text: inout String) throws -> [TextEdit] {
        var out: [TextEdit] = []
        out.reserveCapacity(edits.count)
        for edit in edits {
            guard edit.to >= edit.from else { throw TextOffsetError.outOfRange(edit.to) }
            let start = try index(of: edit.from, in: text)
            let end = try index(of: edit.to, in: text)
            let utf8 = text.utf8
            out.append(
                TextEdit(
                    start: UInt64(utf8.distance(from: utf8.startIndex, to: start)),
                    end: UInt64(utf8.distance(from: utf8.startIndex, to: end)),
                    text: edit.insert))
            text.replaceSubrange(start..<end, with: edit.insert)
        }
        return out
    }

    /// The index of UTF-16 offset `offset` in `text`.
    public static func index(of offset: Int, in text: String) throws -> String.Index {
        let utf16 = text.utf16
        guard offset >= 0,
            let i = utf16.index(utf16.startIndex, offsetBy: offset, limitedBy: utf16.endIndex)
        else { throw TextOffsetError.outOfRange(offset) }
        // Replacing at an index inside a surrogate pair would leave half a
        // character behind; String would repair it with U+FFFD, silently
        // changing the text the core sees.
        guard i.samePosition(in: text.unicodeScalars) != nil else {
            throw TextOffsetError.splitsCharacter(offset)
        }
        return i
    }
}

/// A text's lines, for turning the core's positions (1-based lines and
/// 1-based UTF-8 byte columns, `SourceSpan`) into an editor's UTF-16
/// offsets. Lines end at "\n" only, as the core counts them.
public struct SourceLines: Sendable {
    public let text: String
    /// The UTF-8 offset of each line's first byte.
    private let starts: [Int]

    public init(_ text: String) {
        var text = text
        // Offsets into a string bridged from NSString (UTF-16 inside) cost
        // a scan each; into native UTF-8, a subtraction.
        text.makeContiguousUTF8()
        self.text = text
        var starts = [0]
        for (i, byte) in text.utf8.enumerated() where byte == 0x0A {
            starts.append(i + 1)
        }
        self.starts = starts
    }

    public var lineCount: Int { starts.count }

    /// The UTF-16 offset of `line`:`column` (both 1-based, the column in
    /// bytes). A column past the line's end is its end, a line past the
    /// last is the text's end, and a column inside a multi-byte character
    /// is that character's start.
    public func utf16Offset(line: Int, column: Int) -> Int {
        let utf8 = text.utf8
        guard line >= 1 else { return 0 }
        guard line <= starts.count else { return text.utf16.count }
        let lineStart = starts[line - 1]
        // The line's end, before its "\n".
        let lineEnd = line < starts.count ? starts[line] - 1 : utf8.count
        let byte = min(max(lineStart + column - 1, lineStart), lineEnd)
        var i = utf8.index(utf8.startIndex, offsetBy: byte)
        while i.samePosition(in: text.unicodeScalars) == nil {
            i = utf8.index(before: i)
        }
        return text.utf16.distance(from: text.utf16.startIndex, to: i)
    }
}
