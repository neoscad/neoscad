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
}
