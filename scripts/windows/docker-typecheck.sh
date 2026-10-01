#!/usr/bin/env bash
# Type-checks the WinUI app's C# (windows/NeoSCAD.App) off Windows, in the
# .NET SDK container (docs/windows-app.md, "Testing off Windows").
#
# The XAML compiler and the resource tools run only on Windows, so this
# compiles the app's .cs files as a library against the Windows App SDK's
# reference assemblies (EnableWindowsTargeting), with stand-ins for what
# the XAML compiler would generate (scripts/windows/xaml-standins.py: the
# x:Name fields, InitializeComponent, and every event handler the XAML
# names, subscribed to its event so a wrong name or signature fails). It
# proves the C# is well typed against the real WinUI API; it does not
# prove the XAML loads.
#
# Needs the binding: run scripts/windows/docker-test.sh first (it
# generates windows/NeoSCAD.Bindings/Generated). The project and NuGet
# cache live under $NEOSCAD_DOCKER_DIR (default target/docker-windows).
#
#   scripts/windows/docker-typecheck.sh [--platform linux/amd64]
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
platform=()
if [[ "${1:-}" == "--platform" ]]; then
    platform=(--platform "$2")
    shift 2
fi
state=${NEOSCAD_DOCKER_DIR:-$repo/target/docker-windows}
work="$state/typecheck"
rm -rf "$work"
mkdir -p "$work" "$state/nuget"

if [[ ! -f "$repo/windows/NeoSCAD.Bindings/Generated/neoscad_ffi.cs" ]]; then
    echo "the binding is missing: run scripts/windows/docker-test.sh first" >&2
    exit 1
fi

python3 "$repo/scripts/windows/xaml-standins.py" "$work/StandIns.cs" \
    "$repo/windows/NeoSCAD.App/MainWindow.xaml" "$repo/windows/NeoSCAD.App/App.xaml"

# The app's own project settings, less the XAML and packaging tooling.
sdk=$(sed -n 's/.*Include="Microsoft.WindowsAppSDK" Version="\([^"]*\)".*/\1/p' \
    "$repo/windows/NeoSCAD.App/NeoSCAD.App.csproj")
cat > "$work/TypeCheck.csproj" <<EOF
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0-windows10.0.19041.0</TargetFramework>
    <TargetPlatformMinVersion>10.0.17763.0</TargetPlatformMinVersion>
    <EnableWindowsTargeting>true</EnableWindowsTargeting>
    <OutputType>Library</OutputType>
    <RootNamespace>NeoSCAD.App</RootNamespace>
    <LangVersion>latest</LangVersion>
    <Nullable>enable</Nullable>
    <ImplicitUsings>enable</ImplicitUsings>
    <TreatWarningsAsErrors>true</TreatWarningsAsErrors>
    <WindowsPackageType>None</WindowsPackageType>
    <!-- MakePri.exe (the resource index) is a Windows program. -->
    <AppxGeneratePriEnabled>false</AppxGeneratePriEnabled>
    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>
    <Platforms>x64</Platforms>
    <RuntimeIdentifier>win-x64</RuntimeIdentifier>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Microsoft.WindowsAppSDK" Version="$sdk" />
    <ProjectReference Include="/src/windows/NeoSCAD.Host/NeoSCAD.Host.csproj" />
    <Compile Include="/src/windows/NeoSCAD.App/**/*.cs" Exclude="/src/windows/NeoSCAD.App/obj/**;/src/windows/NeoSCAD.App/bin/**" />
    <Compile Include="StandIns.*.cs" />
  </ItemGroup>
</Project>
EOF

docker run --rm ${platform[@]+"${platform[@]}"} --memory 8g --memory-swap 8g \
    -v "$repo":/src -v "$work":/typecheck -v "$state/nuget":/root/.nuget/packages -w /typecheck \
    -e DOTNET_CLI_TELEMETRY_OPTOUT=1 -e DOTNET_NOLOGO=1 \
    mcr.microsoft.com/dotnet/sdk:10.0 \
    dotnet build TypeCheck.csproj -c Release -p:Platform=x64 -nologo
