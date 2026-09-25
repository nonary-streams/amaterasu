[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateRange(1, 6)][int]$Slot,
    [Parameter(Mandatory)][string]$Worktree,
    [Parameter(Mandatory)][string]$ExpectedBranch,
    [Parameter(Mandatory)][string]$ExpectedHead,
    [Parameter(Mandatory)][string]$TaskFile,
    [Parameter(Mandatory)][string]$RunDirectory,
    [ValidateSet('gpt-6-luna')][string]$Model = 'gpt-6-luna',
    [ValidateSet('max')][string]$ReasoningEffort = 'max',
    [ValidateSet('gpt-6-luna')][string]$SubagentModel = 'gpt-6-luna',
    [ValidateSet('xhigh')][string]$SubagentReasoningEffort = 'xhigh',
    [switch]$Coordinator,
    [switch]$PreflightOnly
)

$ErrorActionPreference = 'Stop'
$worktreePath = (Resolve-Path -LiteralPath $Worktree).Path
$taskPath = (Resolve-Path -LiteralPath $TaskFile).Path
$runPath = [IO.Path]::GetFullPath($RunDirectory)
if (!$Coordinator -and (Test-Path -LiteralPath (Join-Path $runPath 'coordinators-required.json'))) {
    throw 'This run now requires six coordinators with three workers each. Read the current configuration-execution-plan.md and use the team run directory with -Coordinator.'
}
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
$role = if ($Coordinator) { 'coordinator' } else { 'worker' }
$identity = [ordered]@{ slot = $Slot; role = $role; worktree = $worktreePath; branch = $ExpectedBranch; base = $ExpectedHead; cargo_target_dir = $targetPath; model = $Model; effort = $ReasoningEffort }
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
    $roleInstructions = if ($Coordinator) { @"
You are one of SIX autonomous feature coordinators. The user explicitly wants
each coordinator to manage up to THREE implementation/review workers. Delegate
bounded work using your native subagent tools; workers must not delegate further.
Own complete feature delivery, local testing, repairs, and independent review.
Use the prepared build-enabled child worktrees in your assignment. Give each
worker its own manifest BuildDirectory/cargo-target and CARGO_BUILD_JOBS=2.
Assign disjoint paths and a feature-level acceptance test. No startup-only
sessions or model verification approval round trips. Verify checkout state in
the same command that starts useful work. You may act on all reserved feature
paths without per-file approval. Escalate only an actual cross-team collision.
The runner pins this coordinator to gpt-6-luna/max and every native worker to
gpt-6-luna/xhigh. Do not override these models or spawn Sol workers. If you are
continuing an older coordinator session launched as Sol, stop assigning new
work and wait for every already-running worker to finish and commit. Record
their SHAs and the remaining task in STATUS.json/INBOX.json, then exit for root
to relaunch a fresh Luna Max codex exec on this same team worktree. The resumed
session reads the mailbox and commits before continuing; do not terminate or
duplicate workers or replay completed tasks.
Keep your workers doing useful work; do not become a mailbox-only manager.
Review worker changes locally, integrate into your team branch, and publish
READY.json in your assigned team mailbox after checks pass. Include base/head,
commit SHAs, exact checks/results, changed paths, independent reviewer, and
remaining platform gates. Worker commits should appear as increments complete.
The central integrator lands ready work; it does not repeat your routine review
or act as your build server. Continue independent feature work while it lands.
Never write main or another team's checkout. Do not push. Preserve old branches.
"@ } else { @"
You are a direct implementation worker. Complete the feature in the task below.
Do not delegate, spawn agents, or launch other Codex processes. Read AGENTS.md.
"@ }
    $instructions = @"
$roleInstructions
Read AGENTS.md and docs/configuration-execution-plan.md.
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
    $agentSettings = if ($Coordinator) {
        @('-c', 'agents.enabled=true', '-c', 'agents.max_concurrent_threads_per_session=3',
          '-c', "agents.default_subagent_model=`"$SubagentModel`"",
          '-c', "agents.default_subagent_reasoning_effort=`"$SubagentReasoningEffort`"")
    } else { @('-c', 'agents.enabled=false') }
    $arguments = @('--ask-for-approval', 'never', 'exec', '--model', $Model,
        '-c', "model_reasoning_effort=`"$ReasoningEffort`"") + $agentSettings + @(
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
