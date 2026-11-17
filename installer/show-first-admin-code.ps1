[CmdletBinding()]
param(
    [string]$DataRoot = (Join-Path $env:ProgramData 'Amaterasu'),
    [switch]$Refresh
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$stateRoot = Join-Path $DataRoot "runtime\user-state\$sid"
$path = Join-Path $stateRoot 'first-admin-enrollment.json'
if ($Refresh) {
    # The state helper takes the state provider's exclusive lock; it refuses to
    # race a running service. It never replaces a configured administrator.
    $result = & (Join-Path $PSScriptRoot 'amaterasu-authentication-import.exe') prepare-enrollment --state $stateRoot
    if ($LASTEXITCODE -ne 0) { throw 'Enrollment refresh failed; the state provider may still be running.' }
    if ($result -eq 'existing_amaterasu_state') { throw 'An administrator is already configured.' }
}
$item = Get-Item -LiteralPath $path -Force
if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $item.Length -gt 4096) {
    throw 'The enrollment handoff is not a regular bounded file.'
}
$handoff = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
if ($handoff.code -cnotmatch '^ama_[0-9a-f]{64}$' -or
    $handoff.expires_at -le [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()) {
    throw 'The enrollment code is invalid or has expired.'
}
Write-Output "Enrollment code: $($handoff.code)"
Write-Output "Expires: $([DateTimeOffset]::FromUnixTimeSeconds([long]$handoff.expires_at).ToLocalTime())"
