# Builds the Lasair installers (FR-H5, PRD A68) with WiX Toolset v5:
#   dist\Lasair-<version>-windows-x64.msi        the app, the CLI, the
#       examples, the bundled CPython (python-build-standalone, pinned in
#       packaging\python-runtime.env and checked by SHA-256) and the AI Host
#   dist\Lasair-<version>-windows-x64-setup.exe  a Burn bundle: the
#       Microsoft Visual C++ runtime, then that MSI (bundle.wxs)
# Unsigned by default; signs the executables, the MSI and the bundle when a
# certificate is configured. See docs/install.md.
#
#   pwsh packaging/windows/build-msi.ps1 [-SkipBuild]
#
# Needs: Rust (MSVC), the .NET SDK and WiX v5 with two extensions:
#   dotnet tool install --global wix --version 5.0.2
#   wix extension add -g WixToolset.BootstrapperApplications.wixext/5.0.2 WixToolset.Util.wixext/5.0.2
# (Util: the MSI's quiet uninstall action and the bundle's registry search)
# (the script adds the extensions itself when they are missing).
#
# Environment (all optional):
#   LSR_VERSION                  version (default: the workspace version)
#   LSR_VCREDIST                 path to vc_redist.x64.exe; otherwise it is
#                                downloaded from Microsoft into $LSR_DIST\tools.
#                                Either way it must carry a valid Microsoft
#                                Authenticode signature
#   LSR_VCREDIST_SHA256          also require this SHA-256 of it
#   CARGO_TARGET_DIR             as for cargo (default: target)
#   LSR_DIST                     output folder (default: dist)
#   LSR_WIN_SIGN_THUMBPRINT      SHA-1 thumbprint of a code-signing certificate
#                                in the certificate store (or a hardware token), or:
#   LSR_WIN_SIGN_PFX             path to a .pfx file, with
#   LSR_WIN_SIGN_PFX_PASSWORD    its password
#   LSR_WIN_TIMESTAMP_URL        RFC 3161 timestamp server
#                                (default http://timestamp.digicert.com)
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path "$PSScriptRoot\..\..").Path
Set-Location $root
$version = $env:LSR_VERSION
if (-not $version) {
    $version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value
}
# MSI versions are numeric (major.minor.build): drop a pre-release suffix.
$msiVersion = ($version -split '[-+]')[0]
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$root\target" }
$dist = if ($env:LSR_DIST) { $env:LSR_DIST } else { "$root\dist" }
$bin = "$target\release"
$tools = "$dist\tools"
New-Item -ItemType Directory -Force $tools | Out-Null

# The bundled CPython: download, check against its pin, unpack (once).
$pins = @{}
foreach ($line in Get-Content "$root\packaging\python-runtime.env") {
    if ($line -match '^\s*([A-Za-z0-9_]+)=(.*)$') { $pins[$Matches[1]] = $Matches[2].Trim() }
}
$triple = 'x86_64-pc-windows-msvc'
$pySha = $pins["PBS_SHA256_$($triple -replace '-', '_')"]
if (-not $pySha) { throw "python-runtime.env pins no Python for $triple" }
$pyName = "cpython-$($pins.PBS_PYTHON)+$($pins.PBS_RELEASE)-$triple-install_only_stripped.tar.gz"
$pyArchive = "$tools\$pyName"
if (-not (Test-Path $pyArchive)) {
    $url = "https://github.com/astral-sh/python-build-standalone/releases/download/$($pins.PBS_RELEASE)/" +
        ($pyName -replace '\+', '%2B')
    Write-Host "downloading $pyName"
    Invoke-WebRequest -Uri $url -OutFile "$pyArchive.part" -UseBasicParsing
    Move-Item -Force "$pyArchive.part" $pyArchive
}
$got = (Get-FileHash -Algorithm SHA256 $pyArchive).Hash.ToLowerInvariant()
if ($got -ne $pySha) {
    Remove-Item -Force $pyArchive
    throw "$pyName has SHA-256 $got, expected $pySha (python-runtime.env)"
}
$pyOut = "$tools\python-$($pins.PBS_PYTHON)-$($pins.PBS_RELEASE)-$triple"
$stamp = "$pyOut\.sha256"
if (-not ((Test-Path $stamp) -and ((Get-Content $stamp) -eq $pySha))) {
    Remove-Item -Recurse -Force $pyOut -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $pyOut | Out-Null
    tar -xzf $pyArchive -C $pyOut   # bsdtar, part of Windows 10 and later
    if ($LASTEXITCODE) { throw "unpacking $pyName failed" }
    # No Tk GUI toolkit (tkinter, IDLE, turtle and Tcl/Tk); pip and the C
    # headers stay (Mark, 2026-10-31).
    foreach ($p in 'tcl', 'Lib\tkinter', 'Lib\idlelib', 'Lib\turtledemo', 'Lib\turtle.py',
                   'DLLs\_tkinter.pyd', 'DLLs\tcl*.dll', 'DLLs\tk*.dll') {
        Remove-Item -Recurse -Force "$pyOut\python\$p" -ErrorAction SilentlyContinue
    }
    Set-Content -Path $stamp -Value $pySha
}
$pyHome = "$pyOut\python"
$minor = ($pins.PBS_PYTHON -split '\.')[0..1] -join ''
$pyDll = "python$minor.dll"
if (-not (Test-Path "$pyHome\$pyDll")) { throw "$pyHome\$pyDll is missing" }

if (-not $SkipBuild) {
    # Link against the bundled runtime (python313.lib in its libs\ folder).
    $env:PYO3_PYTHON = "$pyHome\python.exe"
    cargo build --release -p lsr-app -p lsr-cli
    if ($LASTEXITCODE) { throw "cargo build failed" }
}
foreach ($exe in 'lasair-app.exe', 'lasair.exe') {
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

$signing = $env:LSR_WIN_SIGN_THUMBPRINT -or $env:LSR_WIN_SIGN_PFX
function Sign([string]$file) {
    if (-not $signing) { return }
    $tool = Find-SignTool
    $ts = if ($env:LSR_WIN_TIMESTAMP_URL) { $env:LSR_WIN_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
    $signArgs = @('sign', '/fd', 'SHA256', '/td', 'SHA256', '/tr', $ts, '/d', 'Lasair')
    if ($env:LSR_WIN_SIGN_THUMBPRINT) {
        $signArgs += @('/sha1', $env:LSR_WIN_SIGN_THUMBPRINT)
    } else {
        $signArgs += @('/f', $env:LSR_WIN_SIGN_PFX)
        if ($env:LSR_WIN_SIGN_PFX_PASSWORD) { $signArgs += @('/p', $env:LSR_WIN_SIGN_PFX_PASSWORD) }
    }
    & $tool @signArgs $file
    if ($LASTEXITCODE) { throw "signing $file failed" }
}

if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    throw "WiX v5 not found: dotnet tool install --global wix --version 5.0.2"
}
# The bundle's extensions, in WiX's per-user cache (adding one already there
# does nothing).
foreach ($ext in 'WixToolset.BootstrapperApplications.wixext', 'WixToolset.Util.wixext') {
    wix extension add -g "$ext/5.0.2"
    if ($LASTEXITCODE) { throw "wix extension add $ext failed" }
}
wix extension list -g

# The Visual C++ runtime the bundle installs. Microsoft's permalink always
# serves the latest 14.x build, so it is checked by its Authenticode
# signature (and by LSR_VCREDIST_SHA256 when that pins one).
$vcRedist = if ($env:LSR_VCREDIST) { $env:LSR_VCREDIST } else { "$tools\vc_redist.x64.exe" }
if (-not (Test-Path $vcRedist)) {
    Write-Host "downloading vc_redist.x64.exe"
    Invoke-WebRequest -Uri 'https://aka.ms/vs/17/release/vc_redist.x64.exe' -OutFile $vcRedist -UseBasicParsing
}
$vcSig = Get-AuthenticodeSignature $vcRedist
if ($vcSig.Status -ne 'Valid' -or $vcSig.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
    throw "$vcRedist is not validly signed by Microsoft ($($vcSig.Status): $($vcSig.SignerCertificate.Subject))"
}
if ($env:LSR_VCREDIST_SHA256) {
    $vcHash = (Get-FileHash -Algorithm SHA256 $vcRedist).Hash
    if ($vcHash -ne $env:LSR_VCREDIST_SHA256.ToUpperInvariant()) {
        throw "$vcRedist has SHA-256 $vcHash, expected $env:LSR_VCREDIST_SHA256"
    }
}
# 14.44.35211.0 -> 35211, the Bld value its installer writes to the registry.
$vcVersion = (Get-Item $vcRedist).VersionInfo.FileVersion
$vcBld = [int]($vcVersion -split '\.')[2]
Write-Host "VC++ runtime $vcVersion (Bld $vcBld)"

# Sign copies, so the target folder keeps cargo's own files.
$stage = "$dist\stage-windows"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item "$bin\lasair-app.exe", "$bin\lasair.exe" $stage
if ($signing) {
    Sign "$stage\lasair-app.exe"
    Sign "$stage\lasair.exe"
} else {
    Write-Host "no LSR_WIN_SIGN_THUMBPRINT or LSR_WIN_SIGN_PFX: unsigned (SmartScreen will warn)"
}
# The examples without their caches.
Copy-Item -Recurse "$root\examples" "$stage\examples"
Get-ChildItem "$stage\examples" -Recurse -Directory -Force -Filter .cache |
    Remove-Item -Recurse -Force
# The AI Host (as packaging/stage-ai-host.sh): source, lock and the extras'
# hash-pinned requirements, precompiled, so the installed app starts its
# local host with no setup. Extras the user installs go to
# %LOCALAPPDATA%\Lasair\ai-host; no packages or weights are shipped.
$aiHost = "$stage\ai-host"
New-Item -ItemType Directory -Force "$aiHost\src" | Out-Null
Copy-Item "$root\ai-host\pyproject.toml", "$root\ai-host\uv.lock", "$root\ai-host\README.md" $aiHost
Copy-Item -Recurse "$root\ai-host\src\lsr_ai_host" "$aiHost\src\lsr_ai_host"
Copy-Item -Recurse "$root\ai-host\requirements" "$aiHost\requirements"
Get-ChildItem $aiHost -Recurse -Directory -Force -Filter __pycache__ | Remove-Item -Recurse -Force
Get-ChildItem $aiHost -Recurse -File -Filter *.pyc | Remove-Item -Force
& "$pyHome\python.exe" -I -B -m compileall -f -q --invalidation-mode unchecked-hash "$aiHost\src" | Out-Null
if ($LASTEXITCODE) { throw "precompiling the AI Host failed" }
if (Get-ChildItem $aiHost -Recurse -File -Include *.pyd, *.dll, *.exe, *.safetensors, *.onnx, *.bin, *.ckpt, *.pt) {
    throw "$aiHost holds binaries or weights"
}

$msi = "$dist\Lasair-$version-windows-x64.msi"
wix build packaging\windows\lasair.wxs -arch x64 -ext WixToolset.Util.wixext `
    -d "Version=$msiVersion" -d "BinDir=$stage" -d "Root=$root" -d "ExamplesDir=$stage\examples" `
    -d "PythonDir=$pyHome" -d "PythonDll=$pyDll" -d "AiHostDir=$aiHost" -o $msi
if ($LASTEXITCODE) { throw "wix build failed" }
Sign $msi

$setup = "$dist\Lasair-$version-windows-x64-setup.exe"
wix build packaging\windows\bundle.wxs -arch x64 `
    -ext WixToolset.BootstrapperApplications.wixext -ext WixToolset.Util.wixext `
    -d "Version=$msiVersion" -d "Msi=$msi" -d "Root=$root" -d "VcRedist=$vcRedist" -d "VcRedistBld=$vcBld" -o $setup
if ($LASTEXITCODE) { throw "wix build (bundle) failed" }
if ($signing) {
    # A bundle is signed twice: its engine (detached and put back), then the whole.
    $engine = "$stage\engine.exe"
    wix burn detach $setup -engine $engine
    if ($LASTEXITCODE) { throw "wix burn detach failed" }
    Sign $engine
    wix burn reattach $setup -engine $engine -o $setup
    if ($LASTEXITCODE) { throw "wix burn reattach failed" }
    Sign $setup
}
Remove-Item -Recurse -Force $stage
foreach ($f in $msi, $setup) { (Get-FileHash -Algorithm SHA256 $f).Hash + "  " + $f }
