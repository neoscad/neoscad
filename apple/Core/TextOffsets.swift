// Between the editor's offsets and the core's.
//
// CodeMirror, like every JavaScript editor, counts UTF-16 code units; the
// core (`TextEdit`) counts UTF-8 bytes. The two agree on ASCII and nowhere
// else: "é" is one unit and two bytes, "漢" one and three, "😀" two and
// four. Every edit from the editor crosses here. (Diagnostics reach the
// editor from the language server, already in UTF-16 positions:
// `lang::source::SourceFile::utf16_position` in the core.)

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
