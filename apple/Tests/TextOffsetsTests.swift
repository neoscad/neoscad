// The editor's UTF-16 offsets against the core's UTF-8 ones, with text
// where the two differ: accents (2 bytes, 1 unit), CJK (3 bytes, 1 unit)
// and emoji (4 bytes, 2 units). The edits the conversion produces are
// applied by the core too, which must end with the same text.

import Foundation
import NeoSCADCore
import Testing

@Suite struct TextOffsetsTests {
    @Test func asciiOffsetsAreUnchanged() throws {
        var text = "cube(1);"
        let edits = try TextOffsets.apply([UTF16Edit(from: 5, to: 6, insert: "10")], to: &text)
        #expect(text == "cube(10);")
        #expect(edits == [TextEdit(start: 5, end: 6, text: "10")])
    }

    @Test func emojiAndCJKShiftTheByteOffsets() throws {
        // "😀" is 2 UTF-16 units and 4 bytes; "漢字" 2 units and 6 bytes.
        var text = "// 😀 漢字\ncube(1);"
        let cube = text.utf16.count - 8  // UTF-16 offset of "cube"
        #expect(cube == 9)
        let edits = try TextOffsets.apply(
            [UTF16Edit(from: cube, to: cube + 4, insert: "sphere")], to: &text)
        #expect(text == "// 😀 漢字\nsphere(1);")
        // 3 ("// ") + 4 + 1 + 6 + 1 ("\n") bytes before "cube".
        #expect(edits == [TextEdit(start: 15, end: 19, text: "sphere")])
    }

    @Test func editsApplyInOrderEachToThePreviousResult() throws {
        // What the editor sends for a multi-cursor edit: last change first,
        // each in the offsets of the text before the transaction.
        var text = "é = 1;\n😀 = 2;\n"
        let edits = try TextOffsets.apply(
            [
                UTF16Edit(from: 12, to: 13, insert: "二"),  // "2" -> "二"
                UTF16Edit(from: 4, to: 5, insert: "10"),  // "1" -> "10"
            ], to: &text)
        #expect(text == "é = 10;\n😀 = 二;\n")
        #expect(
            edits == [
                TextEdit(start: 15, end: 16, text: "二"),
                TextEdit(start: 5, end: 6, text: "10"),
            ])
    }

    @Test func insertingAnEmojiBetweenCharacters() throws {
        var text = "a😀b"
        let edits = try TextOffsets.apply([UTF16Edit(from: 3, to: 3, insert: "🎉")], to: &text)
        #expect(text == "a😀🎉b")
        #expect(edits == [TextEdit(start: 5, end: 5, text: "🎉")])
    }

    @Test func anOffsetInsideASurrogatePairIsRefused() {
        var text = "a😀b"
        #expect(throws: TextOffsetError.splitsCharacter(2)) {
            try TextOffsets.apply([UTF16Edit(from: 2, to: 2, insert: "x")], to: &text)
        }
        #expect(throws: TextOffsetError.outOfRange(9)) {
            try TextOffsets.apply([UTF16Edit(from: 0, to: 9, insert: "")], to: &text)
        }
        #expect(text == "a😀b")
    }

    /// The conversion's edits, applied by the core, give the same text the
    /// editor has: the property the app relies on.
    @Test func theCoreAppliesTheConvertedEdits() throws {
        let engine = try Engine()
        let path = "/NeoSCAD-tests/offsets-\(UUID().uuidString).scad"
        var text = "echo(\"😀\", \"漢字\", \"é\");\n"
        try engine.open(path, text: text)
        // NSString counts UTF-16 units, as the editor does.
        let cjk = (text as NSString).range(of: "漢字").location
        let edits = try TextOffsets.apply(
            [UTF16Edit(from: cjk, to: cjk + 2, insert: "かな🎉")], to: &text)
        let info = try engine.edit(path, edits: edits)
        #expect(info.length == UInt64(text.utf8.count))
        #expect(text == "echo(\"😀\", \"かな🎉\", \"é\");\n")
        // The echo output shows the core's text is the editor's.
        let result = try evaluateSync(engine, path)
        #expect(result.echo == ["ECHO: \"😀\", \"かな🎉\", \"é\""])
        try engine.close(path)
    }

    private func evaluateSync(_ engine: Engine, _ path: String) throws -> Evaluation {
        try engine.core.evaluate(path: path)
    }
}
