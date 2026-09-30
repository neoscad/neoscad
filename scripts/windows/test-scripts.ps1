# Checks the Windows scripts without building anything, on any platform
# pwsh runs on (windows-installer.yml runs it before the MSI build):
#
#   pwsh scripts/windows/test-scripts.ps1 [-Out FILE]
#
#   1. every .ps1 in scripts/windows parses (PowerShell's own parser, so a
#      syntax error fails here rather than an hour into a release run);
#   2. licence-rtf.ps1 turns the repository's LICENSE and a stand-in
#      Windows App SDK licence into RTF holding all three sections, with
#      non-ASCII text escaped. With -Out it also writes that RTF, so a
#      reader (macOS `textutil -convert txt`, WordPad) can open it.
#
# Exits non-zero, naming the failed check, on the first failure.

[CmdletBinding()]
param([string]$Out)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repo = Resolve-Path (Join-Path $PSScriptRoot "../..")
$failures = [System.Collections.Generic.List[string]]::new()
function Assert([bool] $ok, [string] $what) {
    if ($ok) { Write-Host "ok   $what" } else { Write-Host "FAIL $what"; $failures.Add($what) }
}

# 1. Parse only: ParseFile runs nothing, so this is safe for scripts that
# build, install or launch.
foreach ($script in Get-ChildItem -File -Path $PSScriptRoot -Filter *.ps1 | Sort-Object Name) {
    $tokens = $null
    $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($script.FullName, [ref]$tokens, [ref]$errors)
    foreach ($e in $errors) {
        Write-Host "  $($script.Name):$($e.Extent.StartLineNumber): $($e.Message)"
    }
    Assert ($errors.Count -eq 0) "$($script.Name) parses"
}

# 2. The licence page. The stand-in SDK licence has what the real one
# (license.txt in the Microsoft.WindowsAppSDK package) may: CRLF line
# ends, curly quotes, a character outside the BMP (a surrogate pair), a
# tab, and RTF's reserved characters.
. (Join-Path $PSScriptRoot "licence-rtf.ps1")
$gpl = Get-Content -Raw (Join-Path $repo "LICENSE")
$sdk = "MICROSOFT SOFTWARE LICENSE TERMS`r`nMICROSOFT WINDOWS APP SDK`r`n`r`n" +
    "1.`tINSTALLATION AND USE RIGHTS. $([char]0x201C)Distributable Code$([char]0x201D) {see} C:\Program Files " +
    "caf$([char]0xE9) $([char]::ConvertFromUtf32(0x1F600))`r`n"
$rtf = New-LicenceRtf -Version "1.2.3-rc.4" -Gpl $gpl -SdkLicence $sdk

Assert $rtf.StartsWith("{\rtf1\ansi") "starts as RTF"
Assert $rtf.EndsWith("}") "ends with the closing brace"
Assert ($rtf -match '\A[\x09\x0A\x0D\x20-\x7E]*\z') "is pure printable ASCII"
Assert ($rtf.Contains("\b NeoSCAD 1.2.3-rc.4 for Windows\b0")) "has the title with the version"
Assert ($rtf.Contains("NeoSCAD is free software, licensed under the GNU General Public License")) "has the preamble"
Assert ($rtf.Contains("By installing NeoSCAD you agree to the Windows App SDK licence terms")) "has the preamble's agreement"
Assert ($rtf.Contains("\b GNU General Public License\b0")) "has the GPL heading"
Assert ($rtf.Contains("GNU GENERAL PUBLIC LICENSE")) "has the GPL's title line"
Assert ($rtf.Contains("Everyone is permitted to copy and distribute verbatim copies")) "has a GPL body line"
Assert ($rtf.Contains("END OF TERMS AND CONDITIONS")) "has the GPL's end"
Assert ($rtf.Contains("\b Microsoft Windows App SDK\b0")) "has the SDK heading"
Assert ($rtf.Contains("MICROSOFT SOFTWARE LICENSE TERMS\par")) "has the SDK licence, CRLF as \par"
Assert ($rtf.Contains("1.\tab INSTALLATION AND USE RIGHTS.")) "has a tab as \tab"
Assert ($rtf.Contains("\u8220?Distributable Code\u8221?")) "escapes curly quotes as \uN?"
Assert ($rtf.Contains("caf\u233?")) "escapes Latin-1 as \uN?"
Assert ($rtf.Contains("\u-10179?\u-8704?")) "escapes a surrogate pair as two signed \uN?"
Assert ($rtf.Contains("\{see\} C:\\Program Files")) "escapes braces and backslashes"
Assert (-not $rtf.Contains("`r")) "has no carriage returns"
# Braces must balance, or the installer's RichEdit shows a truncated or
# empty page. Escaped braces (\{ \}) and backslashes (\\) don't count.
$depth = 0
$balanced = $true
$plain = $rtf -replace '\\[\\{}]', ''
foreach ($c in $plain.ToCharArray()) {
    if ($c -eq [char]'{') { $depth++ } elseif ($c -eq [char]'}') { $depth-- }
    if ($depth -lt 0) { $balanced = $false }
}
Assert ($balanced -and $depth -eq 0) "braces balance"

$threw = $false
try { [void](New-LicenceRtf -Version "1.0.0" -Gpl $gpl -SdkLicence " `r`n") } catch { $threw = $true }
Assert $threw "refuses an empty SDK licence"

if ($Out) {
    [System.IO.File]::WriteAllText([System.IO.Path]::GetFullPath($Out), $rtf, [System.Text.Encoding]::ASCII)
    Write-Host "wrote $Out"
}
if ($failures.Count -gt 0) { throw "$($failures.Count) check(s) failed: $($failures -join '; ')" }
Write-Host "all checks passed"
