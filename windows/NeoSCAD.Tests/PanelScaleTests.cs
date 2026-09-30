using NeoSCAD.Host;

namespace NeoSCAD.Tests;

public class PanelScaleTests
{
    [Theory]
    [InlineData(1.5, 1.5)]
    [InlineData(0.0, 1.0)]
    [InlineData(-2.0, 1.0)]
    [InlineData(double.NaN, 1.0)]
    [InlineData(double.PositiveInfinity, 1.0)]
    public void AnInvalidScaleIsOne(double scale, double expected) =>
        Assert.Equal(expected, PanelScale.Sane(scale));

    [Fact]
    public void PixelsArePhysicalAndTruncatedAsTheCoreDoes()
    {
        Assert.Equal((1200u, 900u), PanelScale.Pixels(800, 600, 1.5, 1.5));
        // The same pair the core's test checks (crates/ffi/src/viewport/tests.rs).
        Assert.Equal((416u, 175u), PanelScale.Pixels(333.5, 100, 1.25, 1.75));
        Assert.Equal((10u, 0u), PanelScale.Pixels(10, 0, 0, 1));
        Assert.Equal((0u, 0u), PanelScale.Pixels(double.NaN, -5, 1, 1));
    }

    [Fact]
    public void PointerDeltasBecomeTheViewportsPoints()
    {
        // A uniform scale leaves DIPs as they are, at any scale.
        Assert.Equal((3.0, -4.0), PanelScale.PointerDelta(3, -4, 1.5, 1.5));
        Assert.Equal((3.0, -4.0), PanelScale.PointerDelta(3, -4, 0, 0));
        // Taller physical pixels down than across: a DIP down is more points.
        Assert.Equal((3.0, -8.0), PanelScale.PointerDelta(3, -4, 1, 2));
    }
}
