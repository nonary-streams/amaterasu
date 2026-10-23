[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [string]$MsBuild = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\MSBuild\Current\Bin\MSBuild.exe',
    [string]$InfVerif = "$env:USERPROFILE\.nuget\packages\microsoft.windows.wdk.x64\10.0.26100.6584\c\tools\10.0.26100.0\x64\infverif.exe",
    [string]$Inf2Cat = "$env:USERPROFILE\.nuget\packages\microsoft.windows.wdk.x64\10.0.26100.6584\c\bin\10.0.26100.0\x86\Inf2Cat.exe",
    [switch]$WithoutPdb
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$output = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $output) {
    throw 'The VHF package output must be a new directory; existing packages are never replaced.'
}
foreach ($tool in @($MsBuild, $InfVerif, $Inf2Cat)) {
    if (-not (Test-Path -LiteralPath $tool -PathType Leaf)) {
        throw "Required local development tool is unavailable: $tool"
    }
}

$driverProject = Join-Path $repository 'drivers\windows-vhf-keyboard-mouse\AmaterasuVhfInput.vcxproj'
$buildArguments = @($driverProject, '/t:Rebuild', '/p:Configuration=Release', '/p:Platform=x64', '/m:1', '/v:minimal')
if ($WithoutPdb) {
    $buildArguments += '/p:LocalBuildWithoutPdb=true'
}
& $MsBuild @buildArguments
if ($LASTEXITCODE -ne 0) { throw 'VHF driver build failed.' }

Push-Location $repository
try {
    & cargo build -p amaterasu-input-companion --release --locked --bin amaterasu-input-companion
    if ($LASTEXITCODE -ne 0) { throw 'VHF companion build failed.' }
}
finally {
    Pop-Location
}

$driverSource = Join-Path $repository 'drivers\windows-vhf-keyboard-mouse'
$driverOutput = Join-Path $driverSource 'build\x64\Release\AmaterasuVhfInput.dll'
$cargoTarget = if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    Join-Path $repository 'target'
}
elseif ([IO.Path]::IsPathFullyQualified($env:CARGO_TARGET_DIR)) {
    $env:CARGO_TARGET_DIR
}
else {
    Join-Path $repository $env:CARGO_TARGET_DIR
}
$companionOutput = Join-Path $cargoTarget 'release\amaterasu-input-companion.exe'
if (-not (Test-Path -LiteralPath $driverOutput -PathType Leaf) -or
    -not (Test-Path -LiteralPath $companionOutput -PathType Leaf)) {
    throw 'Required VHF build output is missing.'
}

$driverStage = Join-Path $output 'driver'
New-Item -ItemType Directory -Path $driverStage -Force | Out-Null
Copy-Item -LiteralPath $driverOutput -Destination (Join-Path $driverStage 'AmaterasuVhfInput.dll')
Copy-Item -LiteralPath $companionOutput -Destination (Join-Path $output 'amaterasu-input-companion.exe')
$template = Get-Content -LiteralPath (Join-Path $driverSource 'AmaterasuVhfInput.inf.in') -Raw
if ($template -notlike '*$ARCH$*') { throw 'VHF INF architecture marker is missing.' }
$inf = $template.Replace('$ARCH$', 'amd64')
[IO.File]::WriteAllText((Join-Path $driverStage 'AmaterasuVhfInput.inf'), $inf, [Text.UTF8Encoding]::new($false))
& $InfVerif /w (Join-Path $driverStage 'AmaterasuVhfInput.inf')
if ($LASTEXITCODE -ne 0) { throw 'VHF INF verification failed.' }
& $Inf2Cat "/driver:$driverStage" /os:10_X64
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath (Join-Path $driverStage 'AmaterasuVhfInput.cat'))) {
    throw 'VHF catalog generation failed.'
}
Write-Output "Prepared unsigned VHF package: $output"
Write-Output 'This output is not installable. Sign the catalog and companion under an explicitly approved identity, then run seal-vhf-package with the independently confirmed fingerprint.'
