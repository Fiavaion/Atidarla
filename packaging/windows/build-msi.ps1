# Builds the Atidarla MSI (FR-H5) with WiX Toolset v5. Unsigned by default;
# signs the executables and the MSI when a certificate is configured.
# See docs/install.md.
#
#   pwsh packaging/windows/build-msi.ps1 [-SkipBuild]
#
# Needs: Rust (MSVC), the .NET SDK and WiX v5:
#   dotnet tool install --global wix --version 5.0.2
#
# Environment (all optional):
#   ATD_VERSION                  version (default: the workspace version)
#   CARGO_TARGET_DIR             as for cargo (default: target)
#   ATD_DIST                     output folder (default: dist)
#   ATD_WIN_SIGN_THUMBPRINT      SHA-1 thumbprint of a code-signing certificate
#                                in the certificate store (or a hardware token), or:
#   ATD_WIN_SIGN_PFX             path to a .pfx file, with
#   ATD_WIN_SIGN_PFX_PASSWORD    its password
#   ATD_WIN_TIMESTAMP_URL        RFC 3161 timestamp server
#                                (default http://timestamp.digicert.com)
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path "$PSScriptRoot\..\..").Path
Set-Location $root
$version = $env:ATD_VERSION
if (-not $version) {
    $version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value
}
# MSI versions are numeric (major.minor.build): drop a pre-release suffix.
$msiVersion = ($version -split '[-+]')[0]
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$root\target" }
$dist = if ($env:ATD_DIST) { $env:ATD_DIST } else { "$root\dist" }
$bin = "$target\release"

if (-not $SkipBuild) {
    cargo build --release -p atd-app -p atd-cli
    if ($LASTEXITCODE) { throw "cargo build failed" }
}
foreach ($exe in 'atidarla-app.exe', 'atidarla.exe') {
    if (-not (Test-Path "$bin\$exe")) { throw "$bin\$exe is missing (build first)" }
}

function Find-SignTool {
    $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $kits = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    $found = Get-ChildItem $kits -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\' } | Sort-Object FullName -Descending |
        Select-Object -First 1
    if (-not $found) { throw "signtool.exe not found (install the Windows SDK)" }
    return $found.FullName
}

$signing = $env:ATD_WIN_SIGN_THUMBPRINT -or $env:ATD_WIN_SIGN_PFX
function Sign([string]$file) {
    if (-not $signing) { return }
    $tool = Find-SignTool
    $ts = if ($env:ATD_WIN_TIMESTAMP_URL) { $env:ATD_WIN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
    $signArgs = @('sign', '/fd', 'SHA256', '/td', 'SHA256', '/tr', $ts, '/d', 'Atidarla')
    if ($env:ATD_WIN_SIGN_THUMBPRINT) {
        $signArgs += @('/sha1', $env:ATD_WIN_SIGN_THUMBPRINT)
    } else {
        $signArgs += @('/f', $env:ATD_WIN_SIGN_PFX)
        if ($env:ATD_WIN_SIGN_PFX_PASSWORD) { $signArgs += @('/p', $env:ATD_WIN_SIGN_PFX_PASSWORD) }
    }
    & $tool @signArgs $file
    if ($LASTEXITCODE) { throw "signing $file failed" }
}

if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    throw "WiX v5 not found: dotnet tool install --global wix --version 5.0.2"
}

# Sign copies, so the target folder keeps cargo's own files.
$stage = "$dist\stage-windows"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "$bin\atidarla-app.exe", "$bin\atidarla.exe" $stage
if ($signing) {
    Sign "$stage\atidarla-app.exe"
    Sign "$stage\atidarla.exe"
} else {
    Write-Host "no ATD_WIN_SIGN_THUMBPRINT or ATD_WIN_SIGN_PFX: unsigned (SmartScreen will warn)"
}

$msi = "$dist\Atidarla-$version-windows-x64.msi"
wix build packaging\windows\atidarla.wxs -arch x64 `
    -d "Version=$msiVersion" -d "BinDir=$stage" -d "Root=$root" -o $msi
if ($LASTEXITCODE) { throw "wix build failed" }
Sign $msi
Remove-Item -Recurse -Force $stage
(Get-FileHash -Algorithm SHA256 $msi).Hash + "  " + $msi
