param([Parameter(Mandatory)][string]$RuntimeRoot,[Parameter(Mandatory)][string]$HostExecutable,[Parameter(Mandatory)][string]$RuntimeSid,[Parameter(Mandatory)][string[]]$PublisherFingerprint)
$ErrorActionPreference='Stop'
$principal=[Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Provisioning requires an administrator.' }
$root=(Resolve-Path -LiteralPath $RuntimeRoot).Path
$hostFile=(Resolve-Path -LiteralPath $HostExecutable).Path
if ($RuntimeSid -notmatch '^S-1-5-21-[0-9-]+$') { throw 'Specify the runtime user SID.' }
foreach ($fingerprint in $PublisherFingerprint) { if ($fingerprint -cnotmatch '^[a-f0-9]{64}$') { throw 'A publisher fingerprint is invalid.' } }
$directory=Join-Path $env:ProgramData 'AmaterasuPluginInstaller'
if (Test-Path -LiteralPath $directory) { if ((Get-Item -LiteralPath $directory).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'The service directory is a reparse point.' } }
New-Item -ItemType Directory -Force -Path $directory | Out-Null
$acl=New-Object Security.AccessControl.DirectorySecurity
$acl.SetAccessRuleProtection($true,$false)
$acl.SetOwner([Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))
foreach ($sid in @('S-1-5-18','S-1-5-32-544')) { $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new($sid),'FullControl','ContainerInherit,ObjectInherit','None','Allow')) }
Set-Acl -LiteralPath $directory -AclObject $acl
$policy=@{repository_directory=(Join-Path (Join-Path (Join-Path $root 'user-state') $RuntimeSid) 'repositories');root=$root;endpoint='\\.\pipe\AmaterasuPluginInstaller-v1';approvals=$directory;host_sha256=(Get-FileHash -LiteralPath $hostFile -Algorithm SHA256).Hash.ToLowerInvariant();runtime_identity=$RuntimeSid;publishers=@($PublisherFingerprint)}
$path=Join-Path $root 'plugin-installation-policy.json'
if (Test-Path -LiteralPath $path) { if ((Get-Item -LiteralPath $path).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'The policy is a reparse point.' } }
[IO.File]::WriteAllText($path,($policy|ConvertTo-Json -Depth 10),[Text.UTF8Encoding]::new($false))
$fileAcl=New-Object Security.AccessControl.FileSecurity
$fileAcl.SetAccessRuleProtection($true,$false)
$fileAcl.SetOwner([Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))
foreach ($sid in @('S-1-5-18','S-1-5-32-544')) { $fileAcl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new($sid),'FullControl','Allow')) }
Set-Acl -LiteralPath $path -AclObject $fileAcl
Write-Output 'Plugin installation policy provisioned. The installed Amaterasu service loads it on its next authorized restart. No driver was installed.'
