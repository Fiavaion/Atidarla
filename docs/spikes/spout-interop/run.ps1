# Spout SDK interop check for atd-interop (Atidarla, MIT OR Apache-2.0).
#
# Builds the peers (peer_*.cpp, next to this script) against the official
# Spout2 SDK source, builds the atd-interop examples, then runs both
# directions, the sender-list checks and the timings, printing the results.
#
# usage: powershell -ExecutionPolicy Bypass -File run.ps1 -Sdk <Spout2\SPOUTSDK folder> [-Out <dir>]
#   -Out: where the peers are built and logs go (default <repo>\.scratch\spout-peers).
# Needs MSVC x64 build tools and a DX12 GPU. Uses sender names "atd-interop-*".
param(
    [Parameter(Mandatory = $true)][string]$Sdk,
    [string]$Out
)
$ErrorActionPreference = "Stop"
$Src = $PSScriptRoot
$Repo = (git -C $Src rev-parse --show-toplevel).Trim()
if (-not $Out) { $Out = Join-Path $Repo ".scratch\spout-peers" }
New-Item -ItemType Directory -Force (Join-Path $Out "logs") | Out-Null
$Logs = Join-Path $Out "logs"
$Ex = Join-Path $Repo "target\release\examples"

Write-Host "== building examples and peers"
Push-Location $Repo
cargo build -q -p atd-interop --release --examples
if ($LASTEXITCODE) { throw "cargo build failed" }
Pop-Location
cmd /c "`"$Src\build.bat`" `"$Sdk`" `"$Out`"" | Select-String "error|built peers"
if ($LASTEXITCODE) { throw "peer build failed" }
$Send = Join-Path $Ex "spout_send.exe"
$Recv = Join-Path $Ex "spout_receive.exe"
$PSend = Join-Path $Out "peer_send.exe"
$PRecv = Join-Path $Out "peer_receive.exe"
$PList = Join-Path $Out "peer_list.exe"

function Start-Bg($exe, [string[]]$argv, $log) {
    Start-Process -FilePath $exe -ArgumentList $argv -NoNewWindow -PassThru `
        -RedirectStandardOutput (Join-Path $Logs "$log.log") -RedirectStandardError (Join-Path $Logs "$log.err")
}
function Show($log, $pattern) {
    Get-Content (Join-Path $Logs "$log.log") | Select-String $pattern | ForEach-Object { "  bg> $_" }
}
function Lists($label) {
    Write-Host "-- $label"
    & $PList | ForEach-Object { "  sdk> $_" }
    & $Recv - 0 | Select-String "senders|active" | ForEach-Object { "  ours> $_" }
}
$Keep = "connected|sender |verified|ms|resiz|name|sent|BAD|gone|waiting"

Write-Host "== 1. our sender -> SDK receiver"
foreach ($case in @(@("atd-interop-d1", "1920", "1080", "--resize"), @("atd-interop-d1rgba", "1280", "720", "--rgba"))) {
    $p = Start-Bg $Send (@($case[0], "8", $case[1], $case[2], "--pattern") + $case[3]) "d1"
    Start-Sleep 2
    & $PRecv $case[0] 7 | Select-String $Keep
    $p.WaitForExit(); Show "d1" "sending|resiz|sent"
}
Write-Host "== 2. SDK sender -> our receiver"
foreach ($case in @(@("atd-interop-d2", "1920", "1080", "--resize"), @("atd-interop-d2rgba", "1280", "720", "--rgba"))) {
    $p = Start-Bg $PSend @($case[0], "8", $case[1], $case[2], $case[3]) "d2"
    Start-Sleep 2
    & $Recv $case[0] 7 --verify | Select-String $Keep
    $p.WaitForExit(); Show "d2" "name|resiz|sent"
}
Write-Host "== 3a. same name: ours first, SDK second"
$a = Start-Bg $Send @("atd-interop-dup", "9", "640", "360", "--pattern") "dupa1"
Start-Sleep 1.5
$b = Start-Bg $PSend @("atd-interop-dup", "4", "640", "360") "dupa2"
Start-Sleep 1.5
Lists "both running (expect atd-interop-dup and atd-interop-dup_1, the SDK one active)"
$b.WaitForExit(); Lists "SDK sender gone (expect atd-interop-dup active)"
$a.WaitForExit(); Lists "both gone (expect an empty list)"
Show "dupa1" "sending"; Show "dupa2" "name"
Write-Host "== 3b. same name: SDK first, ours second"
$a = Start-Bg $PSend @("atd-interop-dup2", "9", "640", "360") "dupb1"
Start-Sleep 1.5
$b = Start-Bg $Send @("atd-interop-dup2", "4", "640", "360", "--pattern") "dupb2"
Start-Sleep 2
Lists "both running (expect atd-interop-dup2_1 = ours, active)"
Write-Host "-- SDK receiver following the active sender"
& $PRecv - 1 | Select-String "connected|verified"
$b.WaitForExit(); Lists "our sender gone (expect atd-interop-dup2 active)"
$a.WaitForExit(); Lists "both gone (expect an empty list)"
Show "dupb2" "sending"; Show "dupb1" "name"
Write-Host "== 4. timings (--timing: only the 16-byte stamp is uploaded per frame)"
foreach ($size in @(@("1920", "1080"), @("3840", "2160"))) {
    Write-Host "-- ours -> SDK $($size[0])x$($size[1])"
    $p = Start-Bg $Send @("atd-interop-t1", "9", $size[0], $size[1], "--timing") "t1"
    Start-Sleep 2
    & $PRecv atd-interop-t1 6 | Select-String "verified|ms"
    $p.WaitForExit(); Show "t1" "sent"
    Write-Host "-- SDK -> ours $($size[0])x$($size[1]) (our receiver following the active sender)"
    $p = Start-Bg $PSend @("atd-interop-t2", "9", $size[0], $size[1], "--timing") "t2"
    Start-Sleep 2
    & $Recv - 6 --verify | Select-String "^sender|verified|ms"
    $p.WaitForExit(); Show "t2" "sent"
}
Lists "end (expect an empty list)"
