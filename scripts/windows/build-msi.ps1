# Builds the Windows app's MSI (windows/installer/NeoSCAD.wxs;
# docs/windows-app.md, "Installer"), after scripts/windows/build-core.ps1:
#
#   pwsh scripts/windows/build-msi.ps1 [-Arch x64|arm64] [-Out DIR]
#
#   1. `dotnet publish` the app, self-contained, into DIR/stage-<arch>/app;
#   2. stage LICENSE, NOTICE and packaging/licenses beside it, and under
#      licenses/third-party/ the licence and notice files of every NuGet
#      package the app was restored from (the .NET runtime pack and the
#      Windows App SDK among them, both of which ship inside the app);
#   3. `wix build` (WiX 5.0.2, installed as a .NET tool into DIR/tools),
#      with a licence page (WixUI_Minimal) generated as DIR/License.rtf
#      -> DIR/NeoSCAD-<version>-windows-<arch>.msi
#
# The version is Cargo.toml's workspace version, as for the exe
# (windows/Directory.Build.props); the MSI's ProductVersion takes only its
# numeric part. The MSI is not signed (docs/release.md, "Windows is
# unsigned").

[CmdletBinding()]
param(
    [ValidateSet("x64", "arm64")]
    [string]$Arch = $(if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }),
    [string]$Out = "dist/windows"
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# Pinned: WiX 6 and later require accepting the Open Source Maintenance Fee
# EULA (their README, "Open Source Maintenance Fee"); 5.0.2 is the last v5.
$WixVersion = "5.0.2"

$repo = Resolve-Path (Join-Path $PSScriptRoot "../..")
$rid = "win-$Arch"
$platform = if ($Arch -eq "arm64") { "ARM64" } else { "x64" }
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Out = Resolve-Path $Out

$cargo = Get-Content -Raw (Join-Path $repo "Cargo.toml")
$match = [regex]::Match($cargo, '(?<=\[workspace\.package\]\s*\nversion\s*=\s*")[^"]+')
if (-not $match.Success) { throw "no version in Cargo.toml's [workspace.package]" }
$version = $match.Value
$numeric = [regex]::Match($version, '^\d+\.\d+\.\d+').Value
Write-Host "NeoSCAD $version ($numeric) for $rid"

# 1. The app. A clean stage each time: a file left from an earlier build
# would be harvested into the MSI.
$stage = Join-Path $Out "stage-$Arch"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
$app = Join-Path $stage "app"
$project = Join-Path $repo "windows/NeoSCAD.App/NeoSCAD.App.csproj"
dotnet publish $project -c Release -r $rid -p:Platform=$platform -p:NeoScadRid=$rid -o $app
if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed ($LASTEXITCODE)" }
if (-not (Test-Path (Join-Path $app "NeoSCAD.exe"))) { throw "publish made no NeoSCAD.exe" }
if (-not (Test-Path (Join-Path $app "Editor/editor.html"))) {
    throw "the editor bundle is missing from the publish (run build-core.ps1 without -SkipEditor)"
}
# Debug symbols stay out of the installer; the CI artifacts keep the build.
Get-ChildItem -Recurse -Path $app -Filter *.pdb | Remove-Item -Force

# 2. Licences. NeoSCAD's own, as in every other artifact
# (packaging/licenses/README.md), then the packages'.
Copy-Item (Join-Path $repo "LICENSE"), (Join-Path $repo "NOTICE") $app
$licenses = Join-Path $app "licenses"
Copy-Item -Recurse (Join-Path $repo "packaging/licenses") $licenses
$thirdParty = Join-Path $licenses "third-party"
New-Item -ItemType Directory -Force -Path $thirdParty | Out-Null

# The restore's record of every package, including the runtime pack a
# self-contained publish downloads (downloadDependencies). Each package's
# licence and notice files sit at its root in the NuGet folder. Copying
# them all, build-only packages included, errs on the side of a notice too
# many; the Windows App SDK's own terms require keeping Microsoft's notices.
$assets = Get-Content -Raw (Join-Path $repo "windows/NeoSCAD.App/obj/project.assets.json") | ConvertFrom-Json
$folders = @($assets.packageFolders.PSObject.Properties.Name)
# A runtime pack that came with the SDK is in its packs folder instead,
# as packs/<Name>/<version>; the NuGet folders use lower case.
$packs = Join-Path (Split-Path -Parent (Get-Command dotnet).Source) "packs"
$packages = [System.Collections.Generic.SortedDictionary[string, string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($p in $assets.libraries.PSObject.Properties) {
    if ($p.Value.type -ne "package") { continue }
    $packages[$p.Value.path] = $p.Name
}
foreach ($framework in $assets.project.frameworks.PSObject.Properties) {
    if (-not ($framework.Value.PSObject.Properties.Name -contains "downloadDependencies")) { continue }
    # Only the two packs that end up in the app: the runtime and the
    # apphost NeoSCAD.exe is made from. The SDK also downloads packs the
    # app never ships (ASP.NET Core, Windows Desktop, the build machine's
    # own RID), whose notices would only mislead.
    $shipped = "Microsoft.NETCore.App.Runtime.$rid", "Microsoft.NETCore.App.Host.$rid"
    foreach ($d in $framework.Value.downloadDependencies) {
        if ($shipped -notcontains $d.name) { continue }
        $v = $d.version.Trim("[", "]").Split(",")[0].Trim()
        $packages["$($d.name.ToLowerInvariant())/$v"] = "$($d.name)/$v"
    }
}
$index = @("Licence and notice files of the NuGet packages NeoSCAD for Windows was built from",
    "(scripts/windows/build-msi.ps1). The .NET runtime and the Windows App SDK",
    "are redistributed inside the app; see each folder.", "")
$licencePattern = '^(license|licence|notice|third-?party-?notices)([._-].*)?$'
foreach ($entry in $packages.GetEnumerator()) {
    $dir = $null
    $candidates = @($folders | ForEach-Object { Join-Path $_ $entry.Key }) + @(Join-Path $packs $entry.Value)
    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) { $dir = $candidate; break }
    }
    if (-not $dir) { throw "package $($entry.Value) is not in $($candidates -join ', ')" }
    $files = @(Get-ChildItem -File -Path $dir | Where-Object { $_.Name -match $licencePattern })
    if ($files.Count -eq 0) { continue }
    $target = Join-Path $thirdParty ($entry.Value -replace '/', '-')
    New-Item -ItemType Directory -Force -Path $target | Out-Null
    foreach ($file in $files) { Copy-Item $file.FullName $target }
    $index += "$($entry.Value): $(($files | ForEach-Object Name) -join ', ')"
}
# Nothing from the Windows App SDK or the runtime pack means the layout
# of the NuGet folder or of project.assets.json changed; fail rather than
# ship without the notices.
foreach ($required in "Microsoft.WindowsAppSDK/", "Microsoft.NETCore.App.Runtime.$rid/") {
    if (-not ($index | Where-Object { $_.StartsWith($required, [StringComparison]::OrdinalIgnoreCase) })) {
        throw "no licence files were found for $required*"
    }
}
Set-Content -Path (Join-Path $thirdParty "README.txt") -Value $index -Encoding utf8

# The installer's licence page (WixUI_Minimal): what the user agrees to
# before installing. NeoSCAD's GPL, then the Windows App SDK's licence,
# whose section 3.b.ii requires that end users of a redistribution agree
# to terms protecting it and Microsoft at least as much as it does. Beside
# the MSI, not in the app folder, so the harvest doesn't install it twice
# (the same texts are under licenses\).
# The package's own folder (Microsoft.WindowsAppSDK-<version>), not
# .Base/.Runtime/.WinUI: a wildcard path would match the folder itself.
$sdkFolder = Get-ChildItem -Directory -Path $thirdParty -Filter "Microsoft.WindowsAppSDK-*" | Select-Object -First 1
$sdkLicence = if ($sdkFolder) {
    Get-ChildItem -File -Path $sdkFolder.FullName | Where-Object { $_.Name -match '^licen[cs]e' } | Select-Object -First 1
}
if (-not $sdkLicence) { throw "the Windows App SDK's licence file was not staged" }
function ConvertTo-RtfText([string] $text) {
    $b = [System.Text.StringBuilder]::new()
    foreach ($c in $text.Replace("`r`n", "`n").ToCharArray()) {
        switch ($c) {
            '\' { [void]$b.Append('\\') }
            '{' { [void]$b.Append('\{') }
            '}' { [void]$b.Append('\}') }
            "`n" { [void]$b.Append("\par`n") }
            default {
                if ([int]$c -lt 128) { [void]$b.Append($c) }
                else {
                    # RTF's \u takes a signed 16-bit code unit.
                    $n = [int]$c
                    if ($n -gt 32767) { $n -= 65536 }
                    [void]$b.Append("\u$n?")
                }
            }
        }
    }
    $b.ToString()
}
$preamble = @"
NeoSCAD $version for Windows

NeoSCAD is free software, licensed under the GNU General Public License, version 2 or (at your option) any later version; its text follows. The source code is at https://github.com/neoscad/neoscad.

NeoSCAD for Windows includes the Microsoft .NET runtime (MIT licence) and the Microsoft Windows App SDK, redistributed under Microsoft's terms, which also follow. By installing NeoSCAD you agree to the Windows App SDK licence terms below as they apply to those components. The licences and notices of every included component are installed in the licenses folder next to the app.
"@
$rtf = "{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fswiss Segoe UI;}{\f1\fmodern Consolas;}}\fs18`n" +
    "\b " + (ConvertTo-RtfText $preamble.Split("`n")[0]) + "\b0\par`n" +
    (ConvertTo-RtfText ($preamble.Substring($preamble.IndexOf("`n") + 1))) + "\par`n" +
    "\b GNU General Public License\b0\par\f1\fs16`n" +
    (ConvertTo-RtfText (Get-Content -Raw (Join-Path $repo "LICENSE"))) + "\par\f0\fs18`n" +
    "\b Microsoft Windows App SDK\b0\par\f1\fs16`n" +
    (ConvertTo-RtfText (Get-Content -Raw $sdkLicence.FullName)) + "}"
$licenceRtf = Join-Path $Out "License.rtf"
[System.IO.File]::WriteAllText($licenceRtf, $rtf, [System.Text.Encoding]::ASCII)

# 3. The MSI.
$tools = Join-Path $Out "tools"
$wix = Join-Path $tools "wix.exe"
if (-not (Test-Path $wix)) {
    dotnet tool install wix --version $WixVersion --tool-path $tools
    if ($LASTEXITCODE -ne 0) { throw "installing WiX $WixVersion failed" }
}
# The licence page's dialogs. `wix extension add` caches the extension
# under .wix\ in the current directory, where `wix build -ext` looks.
$msi = Join-Path $Out "NeoSCAD-$version-windows-$Arch.msi"
Push-Location $Out
try {
    & $wix extension add "WixToolset.UI.wixext/$WixVersion"
    if ($LASTEXITCODE -ne 0) { throw "adding WixToolset.UI.wixext $WixVersion failed" }
    & $wix build (Join-Path $repo "windows/installer/NeoSCAD.wxs") -arch $Arch -d "Version=$numeric" `
        -d "LicenceRtf=$licenceRtf" -ext WixToolset.UI.wixext -bindpath "app=$app" -o $msi
    if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)" }
}
finally {
    Pop-Location
}
Write-Host "built $msi"
if ($env:GITHUB_OUTPUT) { "msi=$msi" | Out-File -FilePath $env:GITHUB_OUTPUT -Append -Encoding utf8 }
