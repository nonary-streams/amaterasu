[CmdletBinding()]
param(
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version = '0.1.0',
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repository 'target\installer'
}
$output = [System.IO.Path]::GetFullPath($OutputDirectory)
$stage = Join-Path $output 'stage'
$wix = 'C:\Program Files (x86)\WiX Toolset v3.14\bin'
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path (Join-Path $cargoBin 'cargo.exe')) {
    $env:Path = "$cargoBin;$env:Path"
}

foreach ($tool in 'heat.exe', 'candle.exe', 'light.exe') {
    if (-not (Test-Path (Join-Path $wix $tool))) {
        throw "WiX Toolset 3.14 is required; missing $tool."
    }
}
if (-not (Test-Path (Join-Path $wix 'WixBalExtension.dll'))) {
    throw 'WiX Bal extension is required to build the Setup bundle.'
}

Push-Location $repository
try {
    & cargo build --workspace --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }

    & npm --prefix frontend run build
    if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed.' }
}
finally {
    Pop-Location
}

if (Test-Path $stage) {
    Remove-Item -LiteralPath $stage -Recurse -Force
}
New-Item -ItemType Directory -Path (Join-Path $stage 'tools') -Force | Out-Null

$releaseDirectory = Join-Path $repository 'target\release'
$installerSigner = $env:AMATERASU_INSTALLER_SIGNING_KEY
if ([string]::IsNullOrWhiteSpace($installerSigner)) {
    $signerDirectory = Join-Path $env:LOCALAPPDATA 'Amaterasu\installer'
    $signerFile = Join-Path $signerDirectory 'release-signing-key.hex'
    New-Item -ItemType Directory -Path $signerDirectory -Force | Out-Null
    if (Test-Path $signerFile) {
        $installerSigner = (Get-Content -LiteralPath $signerFile -Raw).Trim()
    }
    else {
        [byte[]]$signerBytes = New-Object byte[] 32
        [System.Security.Cryptography.RandomNumberGenerator]::Fill($signerBytes)
        $installerSigner = [Convert]::ToHexString($signerBytes).ToLowerInvariant()
        [System.IO.File]::WriteAllText($signerFile, $installerSigner + [Environment]::NewLine)
    }
}
$env:AMATERASU_INSTALLER_SIGNING_KEY = $installerSigner
Copy-Item -LiteralPath (Join-Path $releaseDirectory 'amaterasu.exe') -Destination $stage
Copy-Item -LiteralPath (Join-Path $releaseDirectory 'amaterasu-migration.exe') -Destination (Join-Path $stage 'tools\amaterasu-migration.exe')
Copy-Item -LiteralPath (Join-Path $releaseDirectory 'amaterasu-release.exe') -Destination (Join-Path $stage 'tools\amaterasu-release.exe')
Copy-Item -LiteralPath (Join-Path $releaseDirectory 'amaterasu-pairing-import.exe') -Destination (Join-Path $stage 'tools\amaterasu-pairing-import.exe')
Copy-Item -LiteralPath (Join-Path $releaseDirectory 'amaterasu-authentication-import.exe') -Destination (Join-Path $stage 'tools\amaterasu-authentication-import.exe')
& (Join-Path $releaseDirectory 'amaterasu-release.exe') prepare-installer $releaseDirectory (Join-Path $repository 'frontend\dist') (Join-Path $stage 'seed')
if ($LASTEXITCODE -ne 0) { throw 'Installer seed preparation failed.' }
if (Test-Path (Join-Path $stage 'seed\development-key.json')) {
    throw 'Installer seed must not contain a signing private key.'
}

& (Join-Path $wix 'heat.exe') dir (Join-Path $stage 'seed') -cg InstallerSeed -dr MIGRATIONTOOLS -scom -sreg -sfrag -srd -ag -var var.SeedDir -out (Join-Path $output 'InstallerSeed.wxs')
if ($LASTEXITCODE -ne 0) { throw 'WiX seed harvesting failed.' }

$common = @('-nologo', '-arch', 'x64', "-dProductVersion=$Version", "-dStageDir=$stage", '-ext', 'WixUtilExtension')
& (Join-Path $wix 'candle.exe') @common "-dSeedDir=$(Join-Path $stage 'seed')" (Join-Path $PSScriptRoot 'Product.wxs') (Join-Path $output 'InstallerSeed.wxs') -out "$output\"
if ($LASTEXITCODE -ne 0) { throw 'WiX compilation failed.' }

$msi = Join-Path $output "Amaterasu-$Version-x64.msi"
& (Join-Path $wix 'light.exe') -nologo -ext WixUtilExtension -out $msi (Join-Path $output 'Product.wixobj') (Join-Path $output 'InstallerSeed.wixobj')
if ($LASTEXITCODE -ne 0) { throw 'WiX linking failed.' }

$bundle = Join-Path $output "Amaterasu-$Version-Setup.exe"
$bundleCommon = @('-nologo', '-arch', 'x64', "-dProductVersion=$Version", "-dProjectDir=$repository", "-dMsiPath=$msi", "-dMigrationToolPath=$(Join-Path $releaseDirectory 'amaterasu-migration.exe')", "-dReleaseToolPath=$(Join-Path $releaseDirectory 'amaterasu-release.exe')", '-ext', 'WixBalExtension')
& (Join-Path $wix 'candle.exe') @bundleCommon (Join-Path $PSScriptRoot 'Bundle.wxs') -out "$output\"
if ($LASTEXITCODE -ne 0) { throw 'WiX bundle compilation failed.' }
& (Join-Path $wix 'light.exe') -nologo -ext WixBalExtension -out $bundle (Join-Path $output 'Bundle.wixobj')
if ($LASTEXITCODE -ne 0) { throw 'WiX bundle linking failed.' }

Write-Output "Built $msi"
Write-Output "Built $bundle"
