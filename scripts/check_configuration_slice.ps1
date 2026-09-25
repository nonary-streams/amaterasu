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

Invoke-Check 'cargo' (@('fmt', '--check') + $packageArguments)
Invoke-Check 'cargo' (@('test', '--locked') + $packageArguments)
Invoke-Check 'cargo' (@('clippy', '--locked', '--all-targets') + $packageArguments + @('--', '-D', 'warnings'))
if ($Frontend) {
    Invoke-Check 'npm' @('--prefix', 'frontend', 'ci')
    Invoke-Check 'npm' @('--prefix', 'frontend', 'test')
    Invoke-Check 'npm' @('--prefix', 'frontend', 'run', 'build')
}
Invoke-Check 'git' @('diff', '--check')
