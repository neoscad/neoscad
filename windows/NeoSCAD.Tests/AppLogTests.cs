using NeoSCAD.Host;

namespace NeoSCAD.Tests;

// AppLog is process-wide state; the collection keeps these tests from
// running in parallel with each other.
[Collection("AppLog")]
public class AppLogTests
{
    [Fact]
    public void LinesAreTimestampedAndOneLineEach()
    {
        var line = AppLog.Format(new DateTime(2026, 9, 30, 12, 0, 1, 250, DateTimeKind.Utc),
            TimeSpan.FromMilliseconds(1500), 7, "first\r\nsecond\nthird");
        Assert.Equal("2026-09-30T12:00:01.250Z +1.500s [7] first | second | third", line);
    }

    [Fact]
    public void WritesAppendToTheFileAndStopWhenClosed()
    {
        var dir = Path.Combine(Path.GetTempPath(), "neoscad-applog-" + Guid.NewGuid().ToString("N"));
        var path = Path.Combine(dir, "sub", "app.log");
        try
        {
            AppLog.Write("before open");
            Assert.True(AppLog.Open(path));
            Assert.True(AppLog.Enabled);
            AppLog.Write("one");
            AppLog.Write("boom", new InvalidOperationException("outer", new IOException("inner")));
            AppLog.Close();
            AppLog.Write("after close");
            Assert.False(AppLog.Enabled);

            var lines = File.ReadAllLines(path);
            Assert.Equal(2, lines.Length);
            Assert.EndsWith("] one", lines[0]);
            Assert.Contains("boom: System.InvalidOperationException (0x80131509): outer <- System.IO.IOException", lines[1]);

            // Opening again appends: a relaunch keeps the earlier run's lines.
            Assert.True(AppLog.Open(path));
            AppLog.Write("two");
            AppLog.Close();
            Assert.Equal(3, File.ReadAllLines(path).Length);
        }
        finally
        {
            AppLog.Close();
            if (Directory.Exists(dir)) Directory.Delete(dir, true);
        }
    }

    [Fact]
    public void AFileThatCannotBeOpenedLeavesTheLogOff()
    {
        var dir = Path.Combine(Path.GetTempPath(), "neoscad-applog-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        try
        {
            // A directory cannot be opened for appending.
            Assert.False(AppLog.Open(dir));
            Assert.False(AppLog.Enabled);
        }
        finally
        {
            Directory.Delete(dir, true);
        }
    }
}
