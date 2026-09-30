# Builds what the Windows app (windows/NeoSCAD.sln) needs before
# `dotnet build`, the counterpart of scripts/apple/build-core.sh and
# build-editor.sh (docs/windows-app.md):
#
#   1. the core as a DLL: `cargo rustc --release -p neoscad-ffi
#      --target <triple> --crate-type cdylib` (crates/ffi builds a
#      staticlib for the macOS app; the DLL is asked for here only)
#      -> windows/native/<rid>/neoscad_ffi.dll (+ .pdb)
#   2. the C# binding: uniffi-bindgen-cs, pinned below, on that DLL
#      -> windows/NeoSCAD.Bindings/Generated/neoscad_ffi.cs
#   3. the CodeMirror bundle the editor pane loads, the same one the macOS
#      app ships: `npm ci` and `node build.mjs` in apple/Editor/web
#      -> apple/Editor/web/dist (the app project copies it)
#
#   pwsh scripts/windows/build-core.ps1 [-Arch x64|arm64] [-SkipEditor]
#
# Runs on Windows with Rust (rustup, MSVC build tools) and Node 18+.

[CmdletBinding()]
param(
    [ValidateSet("x64", "arm64")]
    [string]$Arch = $(if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }),
    [switch]$SkipEditor
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# uniffi-bindgen-cs for uniffi 0.32: no release supports it yet (the latest,
# v0.11.0+v0.31.0, targets 0.31, and crates/ffi pins =0.32.2 for the
# Swift binding). This is the head of NordSecurity/uniffi-bindgen-cs#176,
# "Upgrade to uniffi-rs 0.32.0", from its author's fork, pinned by commit.
# Move to the upstream release once it lands (docs/windows-app.md).
$BindgenRepo = "https://github.com/dennisameling/uniffi-bindgen-cs"
$BindgenRev = "0fc022aa1d73fb1dda91a778b63f2824d7dca58b"

$repo = Resolve-Path (Join-Path $PSScriptRoot "../..")
$triple = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$rid = "win-$Arch"
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo "target" }

function Invoke-Checked([string]$what, [scriptblock]$block) {
    & $block
    if ($LASTEXITCODE -ne 0) { throw "$what failed (exit $LASTEXITCODE)" }
}

Push-Location $repo
try {
    Invoke-Checked "rustup target add" { rustup target add $triple }
    Invoke-Checked "cargo rustc (neoscad-ffi)" {
        cargo rustc --locked --release -p neoscad-ffi --target $triple --crate-type cdylib
    }
    $dll = Join-Path $target "$triple/release/neoscad_ffi.dll"
    if (-not (Test-Path $dll)) { throw "no DLL at $dll" }

    $bindgenRoot = Join-Path $target "uniffi-bindgen-cs"
    $bindgen = Join-Path $bindgenRoot "bin/uniffi-bindgen-cs.exe"
    $stamp = Join-Path $bindgenRoot "rev"
    if (-not (Test-Path $bindgen) -or -not (Test-Path $stamp) -or (Get-Content $stamp) -ne $BindgenRev) {
        Invoke-Checked "cargo install uniffi-bindgen-cs" {
            cargo install --locked --force --git $BindgenRepo --rev $BindgenRev uniffi-bindgen-cs --root $bindgenRoot
        }
        Set-Content -Path $stamp -Value $BindgenRev
    }
    $generated = Join-Path $repo "windows/NeoSCAD.Bindings/Generated"
    Invoke-Checked "uniffi-bindgen-cs" {
        & $bindgen --library --no-format --config (Join-Path $repo "windows/uniffi.toml") --out-dir $generated $dll
    }

    $native = Join-Path $repo "windows/native/$rid"
    New-Item -ItemType Directory -Force -Path $native | Out-Null
    Copy-Item $dll $native -Force
    $pdb = [System.IO.Path]::ChangeExtension($dll, ".pdb")
    if (Test-Path $pdb) { Copy-Item $pdb $native -Force }
    Write-Host "build-core: $dll -> $native; binding -> $generated"

    if (-not $SkipEditor) {
        Push-Location (Join-Path $repo "apple/Editor/web")
        try {
            Invoke-Checked "npm ci" { npm ci --no-audit --no-fund --loglevel=error }
            Invoke-Checked "node build.mjs" { node build.mjs }
        } finally {
            Pop-Location
        }
        Write-Host "build-core: editor bundle -> apple/Editor/web/dist"
    }
} finally {
    Pop-Location
}
