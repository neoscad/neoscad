using System.Runtime.InteropServices;
using NeoSCAD.Host;

namespace NeoSCAD.Tests;

public class WebViewRuntimeTests
{
    [Fact]
    public void AnInstalledRuntimeReportsItsVersion()
    {
        Assert.Equal("140.0.3485.54", WebViewRuntime.Installed(() => "140.0.3485.54"));
    }

    [Fact]
    public void EveryWayOfSayingNotInstalledMeansMissing()
    {
        // ERROR_FILE_NOT_FOUND as an HRESULT, what the WinRT projection throws.
        Assert.Null(WebViewRuntime.Installed(() => throw new COMException("not found", unchecked((int)0x80070002))));
        Assert.Null(WebViewRuntime.Installed(() => throw new FileNotFoundException("no runtime")));
        Assert.Null(WebViewRuntime.Installed(() => null));
        Assert.Null(WebViewRuntime.Installed(() => ""));
    }

    [Fact]
    public void OtherFailuresAreNotReportedAsAMissingRuntime()
    {
        Assert.Throws<UnauthorizedAccessException>(() =>
            WebViewRuntime.Installed(() => throw new UnauthorizedAccessException()));
    }

    [Fact]
    public void TheMessageCarriesTheDownloadLink()
    {
        Assert.Contains(WebViewRuntime.DownloadUrl, WebViewRuntime.MissingMessage);
        Assert.StartsWith("https://", WebViewRuntime.DownloadUrl);
    }
}
