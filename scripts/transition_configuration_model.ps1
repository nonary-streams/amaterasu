[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$RunDirectory
)

$ErrorActionPreference = 'Stop'
$runPath = (Resolve-Path -LiteralPath $RunDirectory).Path
$transitionTime = [DateTime]::UtcNow.ToString('o')

for ($slot = 1; $slot -le 6; $slot++) {
    $teamPath = Join-Path $runPath "team-$slot"
    $inboxPath = Join-Path $teamPath 'INBOX.json'
    $backupPath = Join-Path $teamPath 'INBOX.before-sol-transition.json'
    $status = Get-Content -LiteralPath (Join-Path $teamPath 'STATUS.json') -Raw | ConvertFrom-Json -AsHashtable
    $inbox = Get-Content -LiteralPath $inboxPath -Raw | ConvertFrom-Json -AsHashtable

    if (!(Test-Path -LiteralPath $backupPath)) {
        Copy-Item -LiteralPath $inboxPath -Destination $backupPath
    }

    if (!$inbox.ContainsKey('prior_instruction')) {
        $inbox['prior_instruction'] = $inbox['instruction']
    }
    $inbox['message_id'] = "root-sol-transition-team-$slot-20260925"
    $inbox['priority'] = 'immediate'
    $inbox['coordinator'] = 'gpt-6-sol / xhigh'
    $inbox['workers'] = 'gpt-6-sol / medium'
    $inbox['model_policy'] = [ordered]@{
        coordinator = 'gpt-6-sol / xhigh'
        workers = 'gpt-6-sol / medium'
        max_native_workers = 3
        supersedes_prior_model_instructions = $true
    }
    $inbox['instruction'] = @"
Resume this team's current configuration-parity feature as a GPT-6 Sol XHigh coordinator. All newly dispatched implementation and review workers must be GPT-6 Sol Medium and must not delegate. This policy supersedes every Luna instruction preserved in older mailbox entries and task history. Before dispatch, read STATUS.json, this INBOX.json, READY.json, DEPENDENCY.json, task.md, the latest follow-up task, and existing worker commits. The current recorded phase is '$($status['phase'])'; the recorded remaining task is '$($status['remaining_task'])'. Inspect all prepared team and worker checkouts for active writers and dirty paths. Preserve uncommitted work and reservations; let any live legacy worker validate and commit its current increment, then hand off the recorded SHA, checks, remaining work and paths. Do not reset, duplicate a writer, or synchronize a dirty/active checkout. Continue the current bounded feature, review and repair in the owning team, run rustfmt, focused tests and strict all-target Clippy, then publish a current READY.json with base/head, commits, paths, checks, reviewer and remaining platform gates. Root lands ready work on main. Keep CARGO_TARGET_DIR manifest-derived for each worker, CARGO_BUILD_JOBS=2, and serialize host-global tests. Preserve the six-coordinator topology and do not push.
"@
    $inbox['model_transition_utc'] = $transitionTime
    $inbox['prior_model_instructions_superseded'] = $true
    $inbox | ConvertTo-Json -Depth 60 | Set-Content -LiteralPath $inboxPath -Encoding utf8

    $followupPath = Join-Path $teamPath 'followup-task.md'
    if (Test-Path -LiteralPath $followupPath) {
        $followup = Get-Content -LiteralPath $followupPath -Raw
        $followup = $followup.Replace('GPT-6 Luna Max', 'GPT-6 Sol XHigh')
        $followup = $followup.Replace('GPT-6 Luna X-High', 'GPT-6 Sol Medium')
        $followup = $followup.Replace('Luna X-High', 'Sol Medium')
        $followup = $followup.Replace('never dispatch Sol workers', 'never dispatch Luna workers')
        $followup = $followup.Replace('The root runner defaults are now corrected.', 'The current runner pins this model policy.')
        Set-Content -LiteralPath $followupPath -Value $followup -Encoding utf8
    }

    [pscustomobject]@{
        slot = $slot
        phase = $status['phase']
        inbox = $inboxPath
        followup = $followupPath
        model = $inbox['coordinator']
        worker_model = $inbox['workers']
    }
}
