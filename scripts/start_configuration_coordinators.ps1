[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$RunDirectory
)

$ErrorActionPreference = 'Stop'
$runPath = (Resolve-Path -LiteralPath $RunDirectory).Path
$runnerPath = Join-Path $PSScriptRoot 'run_configuration_worker.ps1'
$pwshPath = (Get-Command pwsh -ErrorAction Stop).Source
$timestamp = [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss')

function Read-WorktreeGit([string]$Worktree, [string[]]$GitArguments) {
    $result = & git -c "safe.directory=$Worktree" -C $Worktree @GitArguments
    if ($LASTEXITCODE -ne 0) { throw "Git failed in ${Worktree}: $GitArguments" }
    return ($result -join "`n").Trim()
}

for ($slot = 1; $slot -le 6; $slot++) {
    $teamPath = Join-Path $runPath "team-$slot"
    $worktree = "D:/sources/worktrees/amaterasu-feature-${slot}_large"
    $branch = Read-WorktreeGit $worktree @('branch', '--show-current')
    $slotLockPath = Join-Path $runPath "slot-$slot.lock"
    try {
        $slotLock = [IO.File]::Open($slotLockPath, 'OpenOrCreate', 'ReadWrite', 'None')
        $slotLock.Dispose()
    } catch {
        Write-Warning "Slot $slot remains occupied; leaving its existing coordinator running."
        continue
    }

    $finishedPath = $null
    $latestStart = Get-ChildItem -LiteralPath $runPath -Filter "slot-$slot-*.started.json" |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($latestStart) {
        $finishedPath = $latestStart.FullName.Replace('.started.json', '.finished.json')
        if (!(Test-Path -LiteralPath $finishedPath)) {
            Write-Warning "Slot $slot has an unfinished runner record; inspect it before relaunch."
            continue
        }
    }

    if ($branch -notmatch "^config-throughput/feature-${slot}(?:_|-)") {
        throw "Slot $slot is on an unexpected branch: $branch"
    }
    $head = Read-WorktreeGit $worktree @('rev-parse', 'HEAD')
    $dirty = @(Read-WorktreeGit $worktree @('status', '--porcelain')) | Where-Object { $_ }
    $workers = for ($worker = 1; $worker -le 3; $worker++) {
        $workerTree = "D:/sources/worktrees/amaterasu-team-$slot-worker-${worker}_large"
        [ordered]@{
            number = $worker
            worktree = $workerTree
            branch = Read-WorktreeGit $workerTree @('branch', '--show-current')
            head = Read-WorktreeGit $workerTree @('rev-parse', 'HEAD')
            dirty_paths = @(Read-WorktreeGit $workerTree @('status', '--porcelain')) | Where-Object { $_ }
        }
    }
    $status = Get-Content -LiteralPath (Join-Path $teamPath 'STATUS.json') -Raw | ConvertFrom-Json
    $ready = Get-Content -LiteralPath (Join-Path $teamPath 'READY.json') -Raw | ConvertFrom-Json
    $reservation = (Select-String -LiteralPath (Join-Path $teamPath 'task.md') -Pattern '^Your full recursive write reservations:' | Select-Object -First 1).Line
    [ordered]@{
        recorded_utc = [DateTime]::UtcNow.ToString('o')
        prior_coordinator_finished = $finishedPath
        coordinator_branch = $branch
        coordinator_head = $head
        coordinator_dirty_paths = $dirty
        workers = $workers
        status = $status
        ready = $ready
        reservations = $reservation
        model_transition = 'New coordinator gpt-6-sol/xhigh; new workers gpt-6-sol/medium. Prior sessions and commits retained.'
    } | ConvertTo-Json -Depth 60 | Set-Content -LiteralPath (Join-Path $teamPath 'TRANSITION.json') -Encoding utf8

    $runnerArguments = @(
        '-NoProfile', '-File', $runnerPath, '-Coordinator',
        '-Slot', "$slot", '-Worktree', $worktree,
        '-ExpectedBranch', $branch, '-ExpectedHead', $head,
        '-TaskFile', (Join-Path $teamPath 'task.md'),
        '-RunDirectory', $runPath
    )
    $process = Start-Process -FilePath $pwshPath -ArgumentList $runnerArguments -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $teamPath "sol-$timestamp.stdout.log") `
        -RedirectStandardError (Join-Path $teamPath "sol-$timestamp.stderr.log")
    [pscustomobject]@{ slot = $slot; pid = $process.Id; branch = $branch; head = $head; dirty_paths = $dirty }
}
