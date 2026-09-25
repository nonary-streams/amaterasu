[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateRange(1, 3)][int]$Slot,
    [Parameter(Mandatory)][string]$Worktree,
    [Parameter(Mandatory)][string]$ExpectedBranch,
    [Parameter(Mandatory)][string]$ExpectedHead,
    [Parameter(Mandatory)][string]$TaskFile,
    [Parameter(Mandatory)][string]$RunDirectory,
    [string]$Model = 'gpt-6-sol',
    [string]$ReasoningEffort = 'medium',
    [switch]$PreflightOnly
)

$ErrorActionPreference = 'Stop'
$worktreePath = (Resolve-Path -LiteralPath $Worktree).Path
$taskPath = (Resolve-Path -LiteralPath $TaskFile).Path
$runPath = [IO.Path]::GetFullPath($RunDirectory)
if (!(Split-Path -Leaf $worktreePath).EndsWith('_large') -or !$ExpectedBranch.EndsWith('_large')) {
    throw 'Workers require an explicitly provisioned _large worktree and branch.'
}
$manifestPath = Join-Path (Split-Path -Parent $worktreePath) ('.large-manifests/' + (Split-Path -Leaf $worktreePath) + '.json')
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ([IO.Path]::GetFullPath($manifest.TargetWorktree) -ne [IO.Path]::GetFullPath($worktreePath) -or $manifest.Branch -ne $ExpectedBranch) {
    throw 'The build manifest belongs to a different worktree or branch.'
}
$buildPath = [IO.Path]::GetFullPath($manifest.BuildDirectory)
if (!(Test-Path -LiteralPath $buildPath -PathType Container)) {
    throw 'The manifest build directory is missing.'
}

function Read-Git([string[]]$GitArguments) {
    $result = & git -C $worktreePath @GitArguments
    if ($LASTEXITCODE -ne 0) { throw "Git preflight failed: $GitArguments" }
    return ($result -join "`n").Trim()
}

$actualRoot = [IO.Path]::GetFullPath((Read-Git @('rev-parse', '--show-toplevel')))
if ($actualRoot -ne [IO.Path]::GetFullPath($worktreePath)) { throw 'Not a worktree root.' }
if ((Read-Git @('branch', '--show-current')) -ne $ExpectedBranch) { throw 'Worker branch changed.' }
if ((Read-Git @('rev-parse', 'HEAD')) -ne $ExpectedHead) { throw 'Worker base changed.' }
if (Read-Git @('status', '--porcelain')) { throw 'Worker worktree has uncommitted changes.' }
if (!(Test-Path -LiteralPath (Join-Path $worktreePath 'AGENTS.md'))) { throw 'AGENTS.md is missing.' }

$targetPath = Join-Path $buildPath 'cargo-target'
$identity = [ordered]@{ slot = $Slot; worktree = $worktreePath; branch = $ExpectedBranch; base = $ExpectedHead; cargo_target_dir = $targetPath; model = $Model; effort = $ReasoningEffort }
if ($PreflightOnly) { $identity | ConvertTo-Json; return }

New-Item -ItemType Directory -Path $runPath -Force | Out-Null
# The OS releases this exclusive handle on exit, including a runner crash.
$slotLock = [IO.File]::Open((Join-Path $runPath "slot-$Slot.lock"), 'OpenOrCreate', 'ReadWrite', 'None')
$worktreeLock = $null
$previousTarget = $env:CARGO_TARGET_DIR
$previousJobs = $env:CARGO_BUILD_JOBS
$runId = 'slot-{0}-{1}' -f $Slot, [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss-fff')
$prefix = Join-Path $runPath $runId
try {
    $worktreeLock = [IO.File]::Open((Join-Path $buildPath 'worker.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
    $env:CARGO_TARGET_DIR = $targetPath
    $env:CARGO_BUILD_JOBS = '2'
    $identity['started_utc'] = [DateTime]::UtcNow.ToString('o')
    $identity['runner_pid'] = $PID
    $identity | ConvertTo-Json | Set-Content -LiteralPath "$prefix.started.json" -Encoding utf8
    $instructions = @"
You are a direct implementation worker. Complete the feature in the task below.
Do not delegate, spawn agents, or launch other Codex processes. Read AGENTS.md.
The user approved this build-enabled worktree. Run focused tests, rustfmt and
strict Clippy locally before submitting. CARGO_TARGET_DIR is already isolated;
keep it and CARGO_BUILD_JOBS. Fix your own compile/test/lint failures.
Implement across all assigned feature paths; no startup approval round trip.
If an additional unreserved file is essential, report it to the coordinator;
continue independent work. Never edit another checkout or main.
Commit coherent validated increments using commitlog with actual session
attribution. Do not wait for other workers to commit. Do not push.
Return commit SHAs, changed paths, exact checks/results, and remaining platform
gates. A saved preference without its runtime behavior is not full parity.
Report a concrete blocker after 15 minutes without implementation progress;
do not spend the batch on repeated inventory scans or status documents.

$(Get-Content -LiteralPath $taskPath -Raw)
"@
    $arguments = @('--ask-for-approval', 'never', 'exec', '--model', $Model,
        '-c', "model_reasoning_effort=`"$ReasoningEffort`"", '-c', 'agents.enabled=false',
        '--sandbox', 'danger-full-access', '--cd', $worktreePath, '--json',
        '--output-last-message', "$prefix.result.md", '-')
    $instructions | & codex @arguments 1> "$prefix.events.jsonl" 2> "$prefix.stderr.log"
    $workerExitCode = $LASTEXITCODE
    @{ exit_code = $workerExitCode; finished_utc = [DateTime]::UtcNow.ToString('o'); head = (Read-Git @('rev-parse', 'HEAD')); dirty = [bool](Read-Git @('status', '--porcelain')) } |
        ConvertTo-Json | Set-Content -LiteralPath "$prefix.finished.json" -Encoding utf8
    if ($workerExitCode -ne 0) { throw "Worker exited $workerExitCode; inspect $prefix.stderr.log" }
} finally {
    $env:CARGO_TARGET_DIR = $previousTarget
    $env:CARGO_BUILD_JOBS = $previousJobs
    if ($worktreeLock) { $worktreeLock.Dispose() }
    $slotLock.Dispose()
}
