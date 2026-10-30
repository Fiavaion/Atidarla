<#
Sovereign mode packet-capture hand check on Windows (PRD FR-A8, M4 AC5).

Captures every packet with pktmon while `atidarla render` runs a project with
Sovereign mode on (the default), then lists every remote address seen and
fails if any is outside loopback, private (RFC 1918 / ULA), link-local or
CGNAT ranges. pktmon cannot tell which process sent a packet, so every packet
is judged: run it on a quiet machine.

    cargo build -p atd-cli --release
    # from an elevated PowerShell:
    scripts\sovereign-capture.ps1 [-Project examples\ai-live.atidarla] [-Frames 0-300]
#>
[CmdletBinding()]
param(
    [string]$Project,
    [string]$Frames = "0-300",
    [string]$Atidarla = $env:ATIDARLA
)
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
if (-not $Project) { $Project = Join-Path $root "examples\ai-live.atidarla" }

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) { Write-Error "run from an elevated PowerShell: pktmon needs administrator rights"; exit 2 }
if (-not (Get-Command pktmon -ErrorAction SilentlyContinue)) { Write-Error "pktmon not found (Windows 10 2004 or later)"; exit 2 }

if (-not $Atidarla) {
    foreach ($b in @("target\release\atidarla.exe", "target\debug\atidarla.exe")) {
        $p = Join-Path $root $b
        if (Test-Path $p) { $Atidarla = $p; break }
    }
}
if (-not $Atidarla -or -not (Test-Path $Atidarla)) {
    Write-Error "no atidarla binary: run 'cargo build -p atd-cli --release' or set ATIDARLA"; exit 2
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("atd-sovereign-" + [Guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Path $work | Out-Null
$etl = Join-Path $work "capture.etl"
$txt = Join-Path $work "capture.txt"

pktmon filter remove | Out-Null
pktmon start --capture --comp nics --pkt-size 128 --file-name $etl | Out-Null
$status = 0
try {
    Start-Sleep -Seconds 2
    Write-Host "capturing while: $Atidarla render $Project --range $Frames (Sovereign mode on)"
    & $Atidarla render $Project --out (Join-Path $work "frames") --range $Frames
    $status = $LASTEXITCODE
    Start-Sleep -Seconds 2
} finally {
    pktmon stop | Out-Null
}
if ($status -ne 0) { Write-Warning "the render exited with $status; judging the traffic it made anyway" }
pktmon format $etl -o $txt | Out-Null

function Test-Local([System.Net.IPAddress]$ip) {
    if ($ip.IsIPv4MappedToIPv6) { $ip = $ip.MapToIPv4() }
    if ([System.Net.IPAddress]::IsLoopback($ip)) { return $true }
    $b = $ip.GetAddressBytes()
    if ($ip.AddressFamily -eq [System.Net.Sockets.AddressFamily]::InterNetwork) {
        return ($b[0] -eq 10) -or
            ($b[0] -eq 172 -and $b[1] -ge 16 -and $b[1] -le 31) -or
            ($b[0] -eq 192 -and $b[1] -eq 168) -or
            ($b[0] -eq 169 -and $b[1] -eq 254) -or
            ($b[0] -eq 100 -and $b[1] -ge 64 -and $b[1] -le 127) -or
            ($b[0] -ge 224) -or
            ($b[0] -eq 0)
    }
    # fc00::/7 (ULA), fe80::/10 (link-local), ff00::/8 (multicast), ::
    return (($b[0] -band 0xfe) -eq 0xfc) -or ($b[0] -eq 0xfe -and ($b[1] -band 0xc0) -eq 0x80) -or
        ($b[0] -eq 0xff) -or $ip.Equals([System.Net.IPAddress]::IPv6None)
}

# Lines look like tcpdump's: "... 192.168.1.5.52000 > 140.82.112.4.443: Flags [S] ..."
$flow = [regex]'(?<a>[0-9a-fA-F:.]+)\.\d+ > (?<b>[0-9a-fA-F:.]+)\.\d+:'
$seen = @{}
foreach ($line in Get-Content $txt) {
    foreach ($m in $flow.Matches($line)) {
        foreach ($g in @("a", "b")) {
            $ip = $null
            if ([System.Net.IPAddress]::TryParse($m.Groups[$g].Value, [ref]$ip)) { $seen[$ip.ToString()] = $ip }
        }
    }
}

$bad = @()
Write-Host "remote addresses seen from this machine:"
foreach ($k in ($seen.Keys | Sort-Object)) {
    if (Test-Local $seen[$k]) { Write-Host "  ok   $k" } else { Write-Host "  FAIL $k"; $bad += $k }
}
Write-Host "capture: $etl"
if ($bad.Count -gt 0) {
    Write-Host "FAIL: $($bad.Count) address(es) outside loopback/private/link-local/CGNAT/ULA"
    exit 1
}
Write-Host "PASS: no traffic left this machine and the LAN"
