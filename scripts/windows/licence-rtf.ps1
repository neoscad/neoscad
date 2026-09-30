# The MSI's licence page (WixUI_Minimal's WixUILicenseRtf): a preamble,
# NeoSCAD's GPL and the Windows App SDK's licence, as one RTF document.
# Dot-sourced by build-msi.ps1, which writes it, and by
# test-scripts.ps1, which checks it on any platform pwsh runs on, so what
# the installer shows is tested without building an MSI.
#
#   . scripts/windows/licence-rtf.ps1
#   $rtf = New-LicenceRtf -Version 0.1.0 -Gpl $gplText -SdkLicence $sdkText
#
# The result is pure ASCII: every character outside printable ASCII is an
# RTF escape, so the file can be written with any encoding and read the
# same by the installer's RichEdit control.

# One plain-text block as RTF body text. Line breaks become \par; the
# characters RTF reserves are escaped; anything outside ASCII becomes
# \uN? (N the signed 16-bit UTF-16 code unit, ? the fallback a reader
# without Unicode shows). CRLF is folded first because a Windows checkout
# (core.autocrlf) gives LICENSE and this script's own here-strings CRLF
# endings, and a stray CR would otherwise land in the RTF as a raw control
# character.
function ConvertTo-RtfText([string] $Text) {
    $b = [System.Text.StringBuilder]::new()
    foreach ($c in $Text.Replace("`r`n", "`n").ToCharArray()) {
        $n = [int]$c
        if ($c -eq [char]'\') { [void]$b.Append('\\') }
        elseif ($c -eq [char]'{') { [void]$b.Append('\{') }
        elseif ($c -eq [char]'}') { [void]$b.Append('\}') }
        elseif ($n -eq 10) { [void]$b.Append("\par`n") }
        elseif ($n -eq 9) { [void]$b.Append('\tab ') }
        # Other control characters (a lone CR, a form feed between
        # licence sections) and a byte-order mark carry no text.
        elseif ($n -lt 32 -or $n -eq 127 -or $n -eq 0xFEFF) { }
        elseif ($n -lt 128) { [void]$b.Append($c) }
        else {
            if ($n -gt 32767) { $n -= 65536 }
            [void]$b.Append("\u${n}?")
        }
    }
    $b.ToString()
}

function New-LicenceRtf {
    param(
        [Parameter(Mandatory)] [string] $Version,
        [Parameter(Mandatory)] [string] $Gpl,
        [Parameter(Mandatory)] [string] $SdkLicence
    )
    # Empty licence text would give an installer whose licence page asks
    # the user to agree to nothing; refuse rather than build it.
    foreach ($part in ([ordered]@{ Gpl = $Gpl; SdkLicence = $SdkLicence }).GetEnumerator()) {
        if ([string]::IsNullOrWhiteSpace($part.Value)) { throw "New-LicenceRtf: -$($part.Key) is empty" }
    }
    $title = "NeoSCAD $Version for Windows"
    $preamble = (
        "NeoSCAD is free software, licensed under the GNU General Public License, version 2 or (at your option) any later version; its text follows. The source code is at https://github.com/neoscad/neoscad.",
        "",
        "NeoSCAD for Windows includes the Microsoft .NET runtime (MIT licence) and the Microsoft Windows App SDK, redistributed under Microsoft's terms, which also follow. By installing NeoSCAD you agree to the Windows App SDK licence terms below as they apply to those components. The licences and notices of every included component are installed in the licenses folder next to the app."
    ) -join "`n"
    # Segoe UI for our own text, Consolas for the licences, which are laid
    # out for a fixed-width font. \fs is in half-points.
    $parts = @(
        "{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fswiss Segoe UI;}{\f1\fmodern Consolas;}}\fs18`n"
        "\b $(ConvertTo-RtfText $title)\b0\par\par`n"
        "$(ConvertTo-RtfText $preamble)\par\par`n"
        "\b GNU General Public License\b0\par\f1\fs16`n"
        "$(ConvertTo-RtfText $Gpl)\par\f0\fs18`n"
        "\b Microsoft Windows App SDK\b0\par\f1\fs16`n"
        "$(ConvertTo-RtfText $SdkLicence)}"
    )
    -join $parts
}
