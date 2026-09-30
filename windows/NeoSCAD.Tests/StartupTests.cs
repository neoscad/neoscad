using NeoSCAD.Host;

namespace NeoSCAD.Tests;

public class StartupTests
{
    [Fact]
    public void TheCommandLineOpensAFileOrAnExample()
    {
        Assert.Equal(new StartupAction.Empty(), StartupAction.Parse([]));
        Assert.Equal(new StartupAction.OpenFile(@"C:\m\gear.scad"), StartupAction.Parse([@"C:\m\gear.scad"]));
        Assert.Equal(new StartupAction.OpenExample("gear"), StartupAction.Parse(["--example", "gear"]));
        Assert.Equal(new StartupAction.Empty(), StartupAction.Parse(["--example"]));
    }

    [Fact]
    public void TheLogFileIsNeverOpenedAsAModel()
    {
        // CI runs `NeoSCAD.exe --example csg --log app.log`. Were --log
        // unknown, its value would be the first plain argument, and so the
        // file to open.
        string[] ci = ["--example", "csg", "--log", @"C:\logs\app.log"];
        Assert.Equal(new StartupAction.OpenExample("csg"), StartupAction.Parse(ci));
        Assert.Equal(@"C:\logs\app.log", StartupAction.LogPath(ci));
        Assert.Equal(new StartupAction.Empty(), StartupAction.Parse(["--log", "app.log"]));
        Assert.Equal(new StartupAction.OpenFile("m.scad"), StartupAction.Parse(["--log", "app.log", "m.scad"]));
        Assert.Null(StartupAction.LogPath(["m.scad"]));
        Assert.Null(StartupAction.LogPath(["--log"]));
    }
}
