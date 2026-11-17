param([Parameter(Mandatory)][string]$Msi)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$path = (Resolve-Path -LiteralPath $Msi).Path
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.OpenDatabase($path, 0)
$view = $database.OpenView('SELECT `Action`, `Sequence` FROM `InstallExecuteSequence`')
$view.Execute()
$actions = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
$sequence = @{}
while ($record = $view.Fetch()) {
    $name = $record.StringData(1)
    [void]$actions.Add($name)
    $sequence[$name] = $record.IntegerData(2)
}
$view.Close()

foreach ($name in @('RequireNoPredecessor', 'SetRequireNoPredecessor',
        'ImportLegacyConfiguration', 'SetImportLegacyConfiguration',
        'HandoffPredecessor', 'SetHandoffPredecessor',
        'RollbackPredecessorHandoff', 'SetRollbackPredecessorHandoff',
        'FinalizePredecessorHandoff', 'SetFinalizePredecessorHandoff')) {
    if ($actions.Contains($name)) { throw "Side-by-side MSI schedules predecessor action $name" }
}
foreach ($pair in @(
        @('ValidateInstallerPaths', 'InstallFiles'),
        @('ProvisionInstallerRuntime', 'InitializeSideBySidePairing'),
        @('InitializeSideBySidePairing', 'PrepareSideBySideEnrollment'),
        @('PrepareSideBySideEnrollment', 'StartServices'),
        @('StartServices', 'VerifyAmaterasuOperationalReady'),
        @('VerifyAmaterasuOperationalReady', 'FinalizeInstallerProvision'))) {
    if ($sequence[$pair[0]] -ge $sequence[$pair[1]]) { throw "Invalid installer order: $($pair[0]) then $($pair[1])" }
}
foreach ($name in @('ValidateInstallerPaths', 'ProvisionInstallerRuntime',
        'RollbackProvisionInstallerRuntime', 'InitializeSideBySidePairing', 'PrepareSideBySideEnrollment', 'VerifyAmaterasuServiceReady',
        'VerifyAmaterasuOperationalReady', 'FinalizeInstallerProvision')) {
    if (-not $actions.Contains($name)) { throw "Side-by-side MSI lacks $name" }
}
$view = $database.OpenView('SELECT `Target` FROM `CustomAction` WHERE `Action` = ''SetVerifyAmaterasuOperationalReady''')
$view.Execute()
$record = $view.Fetch()
if (-not $record -or $record.StringData(1) -notmatch 'first-run-ready') {
    throw 'The side-by-side MSI does not select first-run readiness.'
}
$view.Close()
$view = $database.OpenView('SELECT `File` FROM `File` WHERE `File` = ''FirstAdminCodeScript''')
$view.Execute()
if (-not $view.Fetch()) { throw 'The side-by-side MSI does not contain the enrollment handoff tool.' }
$view.Close()
Write-Output 'Side-by-side MSI does not schedule predecessor mutation or import.'
