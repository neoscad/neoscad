// The editor bridge's host side: what the scheme serves, how the page's
// messages are read and how scripts are written. No web view involved.

using System.Text;
using NeoSCAD.Host;
using NeoSCAD.Native;

namespace NeoSCAD.Tests;

public class EditorPageTests
{
    [Fact]
    public void ThePolicyAdmitsOnlyTheBundleAndNoncedStyles()
    {
        Assert.Equal(
            "default-src 'none'; script-src neoscad-editor:; style-src 'nonce-abc'; img-src data:; base-uri 'none'; form-action 'none'",
            EditorPage.ContentSecurityPolicy("abc"));
    }

    [Theory]
    [InlineData("neoscad-editor://app/editor.html", "editor.html")]
    [InlineData("neoscad-editor://app/editor.js", "editor.js")]
    [InlineData("neoscad-editor://other/editor.js", null)]
    [InlineData("https://app/editor.js", null)]
    public void OnlyTheAppHostOfTheSchemeIsServed(string uri, string? name)
    {
        Assert.Equal(name, EditorPage.FileName(uri));
    }

    [Theory]
    [InlineData("editor.html", "text/html; charset=utf-8")]
    [InlineData("editor.js", "text/javascript; charset=utf-8")]
    [InlineData("../secret.js", null)]
    [InlineData("..\\secret.js", null)]
    [InlineData(".hidden.js", null)]
    [InlineData("c:evil.js", null)]
    [InlineData("image.png", null)]
    [InlineData("", null)]
    public void OnlyPlainNamesOfKnownTypesAreServed(string name, string? type)
    {
        Assert.Equal(type, EditorPage.ContentType(name));
    }

    [Fact]
    public void ThePageGetsAFreshNonceInItsBodyAndPolicy()
    {
        var dir = Directory.CreateTempSubdirectory("neoscad-editor-").FullName;
        try
        {
            File.WriteAllText(Path.Combine(dir, "editor.html"), "<style nonce=\"NONCE_PLACEHOLDER\"></style>");
            File.WriteAllText(Path.Combine(dir, "editor.js"), "NONCE_PLACEHOLDER");
            var page = EditorPage.Respond(dir, EditorPage.PageUrl, () => "n1")!;
            Assert.Equal("<style nonce=\"n1\"></style>", Encoding.UTF8.GetString(page.Body));
            Assert.Contains("'nonce-n1'", page.Headers["Content-Security-Policy"]);
            Assert.Equal("no-store", page.Headers["Cache-Control"]);
            // Only the page is rewritten; scripts are served as they are.
            var script = EditorPage.Respond(dir, "neoscad-editor://app/editor.js", () => "n2")!;
            Assert.Equal("NONCE_PLACEHOLDER", Encoding.UTF8.GetString(script.Body));
            Assert.False(script.Headers.ContainsKey("Content-Security-Policy"));
            Assert.Null(EditorPage.Respond(dir, "neoscad-editor://app/missing.js"));
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }

    [Fact]
    public void NoncesDiffer() => Assert.NotEqual(EditorPage.NewNonce(), EditorPage.NewNonce());
}

public class EditorProtocolTests
{
    [Fact]
    public void AChangeIsReadWithItsEditsInOrder()
    {
        var m = EditorMessage.Parse(
            """{"type":"changes","base":3,"version":4,"edits":[[5,6,"x"],[0,0,"ab"]],"kind":"undo","length":9,"undoDepth":2,"redoDepth":1}""");
        var c = Assert.IsType<EditorMessage.Changes>(m);
        Assert.Equal(3, c.Base);
        Assert.Equal(4, c.Version);
        Assert.Equal(EditKind.Undo, c.Kind);
        Assert.Equal([(5UL, 6UL, "x"), (0UL, 0UL, "ab")], c.Edits!);
        Assert.Equal(9, c.Length);
    }

    [Theory]
    [InlineData("""{"type":"changes","base":1,"version":2,"edits":[[1,2]],"length":3}""")]
    [InlineData("""{"type":"changes","base":1,"version":2,"edits":[[-1,2,"x"]],"length":3}""")]
    [InlineData("""{"type":"changes","base":1,"version":2,"edits":[[1,2,3]],"length":3}""")]
    public void AMalformedEditLeavesNoEdits(string json)
    {
        Assert.Null(Assert.IsType<EditorMessage.Changes>(EditorMessage.Parse(json)).Edits);
    }

    [Fact]
    public void TheOtherMessagesAreRead()
    {
        Assert.IsType<EditorMessage.Ready>(EditorMessage.Parse("""{"type":"ready"}"""));
        Assert.Equal(new EditorMessage.Command("render"), EditorMessage.Parse("""{"type":"command","name":"render"}"""));
        Assert.Equal(new EditorMessage.Lsp("{}"), EditorMessage.Parse("""{"type":"lsp","message":"{}"}"""));
        Assert.Equal(new EditorMessage.Open("file:///a.scad", 3, 4),
            EditorMessage.Parse("""{"type":"open","uri":"file:///a.scad","line":3,"character":4}"""));
        Assert.Equal(new EditorMessage.Unknown("new"), EditorMessage.Parse("""{"type":"new"}"""));
        Assert.Null(EditorMessage.Parse("""["ready"]"""));
        Assert.Null(EditorMessage.Parse("not json"));
    }

    [Fact]
    public void ScriptArgumentsAreJsonLiteralsNeverRawText()
    {
        var text = "a\"b'c\n</script>\u2028\u00e9\U0001F600";
        var script = EditorScript.Load(text, null, false);
        Assert.StartsWith("NeoSCADEditor.load(\"", script);
        Assert.EndsWith(", null, false)", script);
        Assert.DoesNotContain("</script>", script);
        Assert.DoesNotContain("\n", script);
        // The literal reads back as the same string.
        var literal = script["NeoSCADEditor.load(".Length..script.IndexOf(", null", StringComparison.Ordinal)];
        Assert.Equal(text, System.Text.Json.JsonSerializer.Deserialize<string>(literal));
    }

    [Fact]
    public void RepliesAreRead()
    {
        Assert.Equal(7, EditorReply.Version("""{"version":7,"undoDepth":0,"redoDepth":0}"""));
        Assert.Equal((2L, "cube();"), EditorReply.Text("""{"version":2,"text":"cube();"}"""));
        Assert.Null(EditorReply.Text("null"));
    }

    [Fact]
    public void WithoutAServerARequestGetsMethodNotFoundAndANotificationNothing()
    {
        Assert.Equal("""{"jsonrpc":"2.0","id":4,"error":{"code":-32601,"message":"No language server yet"}}""",
            JsonRpc.NotFoundReply("""{"jsonrpc":"2.0","id":4,"method":"textDocument/hover"}"""));
        Assert.Null(JsonRpc.NotFoundReply("""{"jsonrpc":"2.0","method":"initialized"}"""));
    }
}

public class EditorSyncTests
{
    static EditorMessage.Changes Change(long b, long v, long length) =>
        new(b, v, [(0UL, 0UL, "x")], EditKind.Edit, length, 1, 0);

    [Fact]
    public void ChangesApplyOnlyToTheVersionTheyWereMadeOn()
    {
        var applied = new List<Utf16Edit[]>();
        var requests = 0;
        var sync = new EditorSync
        {
            Apply = (e, _, _) =>
            {
                applied.Add(e);
                return true;
            },
            RequestText = () => requests++,
        };
        // Before the load's answer nothing applies and nothing is asked.
        sync.Changes(Change(1, 2, 1));
        Assert.Empty(applied);
        sync.Loaded(1);
        sync.Changes(Change(1, 2, 1));
        Assert.Single(applied);
        Assert.Equal(2, sync.Version);
        // A change on another version: the copies disagree, ask for the text.
        sync.Changes(Change(5, 6, 1));
        Assert.Null(sync.Version);
        Assert.Equal(1, requests);
        sync.Resynced(6);
        Assert.Equal(6, sync.Version);
    }

    [Fact]
    public void AChangeThatDoesNotFitAsksForTheWholeText()
    {
        var requests = 0;
        var sync = new EditorSync { Apply = (_, _, _) => false, RequestText = () => requests++ };
        sync.Loaded(1);
        sync.Changes(Change(1, 2, 1));
        Assert.Equal(1, requests);
        Assert.Equal(1, sync.Resyncs);
    }
}

public class TitleTests
{
    [Fact]
    public void AnEditedDocumentHasAnAsterisk()
    {
        Assert.Equal("Untitled - NeoSCAD", Titles.Window("Untitled", false));
        Assert.Equal("*gear.scad - NeoSCAD", Titles.Window("gear.scad", true));
    }
}
