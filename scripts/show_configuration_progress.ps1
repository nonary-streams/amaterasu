[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$RunDirectory,
    [string]$Repository = (Split-Path -Parent $PSScriptRoot)
)

$ErrorActionPreference = 'Stop'
Write-Output 'Recent main commits:'
& git -C $Repository log main -5 --format='%h %cr %s'
if ($LASTEXITCODE -ne 0) { throw 'Cannot read main history.' }
foreach ($slot in 1..6) {
    $latest = Get-ChildItem -LiteralPath $RunDirectory -Filter "slot-$slot-*.started.json" -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 1
    if (!$latest) { Write-Output "Slot ${slot}: not started"; continue }
    $start = Get-Content -LiteralPath $latest.FullName -Raw | ConvertFrom-Json
    $prefix = $latest.FullName -replace '\.started\.json$', ''
    $finishPath = "$prefix.finished.json"
    if (Test-Path -LiteralPath $finishPath) {
        $finish = Get-Content -LiteralPath $finishPath -Raw | ConvertFrom-Json
        $status = "exited $($finish.exit_code); dirty=$($finish.dirty); inspect result before landing"
    } elseif (Get-Process -Id $start.runner_pid -ErrorAction SilentlyContinue) {
        $status = 'running'
    } else {
        $status = 'runner stopped without completion; reconcile before restart'
    }
    $events = Get-Item -LiteralPath "$prefix.events.jsonl" -ErrorAction SilentlyContinue
    $age = if ($events) { [math]::Round(([DateTime]::UtcNow - $events.LastWriteTimeUtc).TotalMinutes, 1) } else { 'unknown' }
    Write-Output "Slot ${slot} ($($start.role)): $status; event age=${age}m; $($start.branch)"
    & git -C $start.worktree log -1 --format='%h %cr %s'
    $candidatePatches = & git -C $start.worktree cherry -v main HEAD
    if ($LASTEXITCODE -ne 0) { throw "Cannot compare slot $slot patches with main." }
    $pending = @($candidatePatches | Where-Object { $_.StartsWith('+ ') })
    if ($pending.Count -eq 0) {
        Write-Output '  pending patches: none'
    } else {
        foreach ($candidate in $pending) {
            Write-Output "  pending patch: $($candidate.Substring(2))"
        }
    }
}
