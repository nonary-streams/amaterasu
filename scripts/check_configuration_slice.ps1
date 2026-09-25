[CmdletBinding()]
param(
    [Parameter(Mandatory)][string[]]$Packages,
    [switch]$Frontend
)

$ErrorActionPreference = 'Stop'
$packageArguments = @()
foreach ($package in $Packages) { $packageArguments += @('-p', $package) }

function Invoke-Check([string]$Program, [string[]]$CheckArguments) {
    & $Program @CheckArguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}

# Bound expensive validation, not agent work. Every team can still implement
# while up to six isolated checkouts build. OS handles recover on process exit.
$lockDirectory = Join-Path ([IO.Path]::GetTempPath()) 'amaterasu-configuration-build-slots'
New-Item -ItemType Directory -Path $lockDirectory -Force | Out-Null
$buildLock = $null
while (!$buildLock) {
    foreach ($buildSlot in 1..6) {
        try {
            $buildLock = [IO.File]::Open((Join-Path $lockDirectory "$buildSlot.lock"), 'OpenOrCreate', 'ReadWrite', 'None')
            break
        } catch [IO.IOException] { }
    }
    if (!$buildLock) { Write-Output 'Waiting for one of six validation slots...'; Start-Sleep -Seconds 1 }
}
try {
    Invoke-Check 'cargo' (@('fmt', '--check') + $packageArguments)
    Invoke-Check 'cargo' (@('test', '--locked') + $packageArguments)
    Invoke-Check 'cargo' (@('clippy', '--locked', '--all-targets') + $packageArguments + @('--', '-D', 'warnings'))
    if ($Frontend) {
        Invoke-Check 'npm' @('--prefix', 'frontend', 'ci')
        Invoke-Check 'npm' @('--prefix', 'frontend', 'test')
        Invoke-Check 'npm' @('--prefix', 'frontend', 'run', 'build')
    }
    Invoke-Check 'git' @('diff', '--check')
} finally {
    $buildLock.Dispose()
}
