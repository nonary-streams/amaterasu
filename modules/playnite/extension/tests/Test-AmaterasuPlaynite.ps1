# Run on Windows: pwsh -NoProfile -File modules/playnite/extension/tests/Test-AmaterasuPlaynite.ps1
# -ModulePath may point to an unmerged extension module in another checkout.
param(
    [string]$ModulePath = (Join-Path $PSScriptRoot '../AmaterasuPlaynite.psm1')
)

$ErrorActionPreference = 'Stop'
trap { Write-Error $_.ScriptStackTrace; break }
$ModulePath = [System.IO.Path]::GetFullPath($ModulePath)
$extensionDirectory = Split-Path -Parent $ModulePath
$manifestPath = Join-Path $extensionDirectory 'extension.yaml'

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-Equal($Actual, $Expected, [string]$Message) {
    if ($Actual -cne $Expected) { throw "$Message (expected '$Expected', got '$Actual')" }
}

function Assert-Throws([scriptblock]$Action, [string]$Message) {
    $threw = $false
    try { & $Action } catch { $threw = $true }
    if (-not $threw) { throw $Message }
}

class MockDispatcher {
    [bool] $Access
    MockDispatcher([bool] $access) { $this.Access = $access }
    [bool] CheckAccess() { return $this.Access }
    [object] Invoke([Func[object]] $action) { return $action.Invoke() }
}

class FailingDispatcher {
    [object] Invoke([Func[object]] $action) { throw 'dispatcher unavailable' }
}

function New-MockApi([bool]$OnUiThread) {
    $categoryId = '50000000-0000-0000-0000-000000000001'
    return [pscustomobject]@{
        MainView = [pscustomobject]@{ UIDispatcher = [MockDispatcher]::new($OnUiThread) }
        Addons = [pscustomobject]@{ Plugins = @([pscustomobject]@{ Id = 'plugin.one'; Name = 'Source' }) }
        Database = [pscustomobject]@{
            Categories = @([pscustomobject]@{ Id = $categoryId; Name = 'Favorites' })
            Games = @([pscustomobject]@{
                Id = '60000000-0000-0000-0000-000000000001'
                Name = 'Mock Game'
                GameActions = @()
                InstallDirectory = ''
                CategoryIds = @($categoryId)
                PluginId = 'plugin.one'
                LastActivity = [datetime]'2026-01-02T03:04:05Z'
                IsInstalled = $true
                IsRunning = $false
            })
        }
    }
}

function New-MockConnection([string]$Nonce, $Cache) {
    return @{
        Nonce = $Nonce
        Cache = $Cache
        Signals = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
        Queue = [System.Collections.Concurrent.BlockingCollection[object]]::new(8)
    }
}

Assert-True (Test-Path -LiteralPath $ModulePath -PathType Leaf) 'extension module is missing'
Assert-True (Test-Path -LiteralPath $manifestPath -PathType Leaf) 'extension.yaml is missing'
$manifest = Get-Content -LiteralPath $manifestPath -Raw
Assert-True ($manifest -match '(?m)^Id:\s*Amaterasu\.Playnite\.Connector\s*$') 'packaging ID changed'
Assert-True ($manifest -match '(?m)^Module:\s*AmaterasuPlaynite\.psm1\s*$') 'packaging module changed'
Assert-True ($manifest -match '(?m)^Type:\s*Script\s*$') 'packaging type changed'
Assert-True ($manifest -match '(?m)^Version:\s*\d+\.\d+\.\d+\s*$') 'packaging version is invalid'

$extension = Import-Module -Name $ModulePath -PassThru -Force
try {
    $coverPath = Join-Path ([System.IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString('N') + '.png')
    try {
        $coverBytes = [byte[]](0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a,1,2,3,4)
        [System.IO.File]::WriteAllBytes($coverPath, $coverBytes)
        $image = & $extension { param($p) $budget = 100L; Read-ArtworkFile $p 2000 ([ref]$budget) } $coverPath
        Assert-True ($null -ne $image) 'valid cover was not read'
        Assert-Equal $image.MimeType 'image/png' 'cover MIME'
        Assert-Equal $image.Revision.Length 64 'cover revision length'
        $noBudget = & $extension { param($p) $budget = 11L; Read-ArtworkFile $p 2000 ([ref]$budget) } $coverPath
        Assert-True ($null -eq $noBudget) 'cover exceeded scan budget'
        $artPayload = @{ Lines=@('{"type":"games","payload":[{"id":"60000000-0000-0000-0000-000000000001"}]}'); Games=1; CapturedAt=[DateTime]::UtcNow; Generation=0L; Artwork=@{ '60000000-0000-0000-0000-000000000001'=@{ State='path'; Path=$coverPath } } }
        $artCache = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
        $artSignals = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
        & $extension { param($payload,$cache,$signals) Scan-ArtworkPayload $payload $cache $signals } $artPayload $artCache $artSignals
        $enriched = & $extension { param($payload,$cache) Add-ArtworkToSnapshot $payload $cache } $artPayload $artCache
        $record = $enriched.Lines[0] | ConvertFrom-Json
        Assert-Equal $record.payload[0].artworkState 'revision' 'cover state'
        Assert-Equal $record.payload[0].artworkRevision $image.Revision 'cover snapshot revision'
        Assert-True (-not $enriched.Lines[0].Contains($coverPath)) 'cover path leaked into snapshot'
        $rotationPayload = @{ Lines=@('{"type":"games","payload":[{"id":"a"},{"id":"b"}]}'); Games=2; CapturedAt=[DateTime]::UtcNow; Generation=0L; Artwork=@{ a=@{ State='path'; Path=$coverPath }; b=@{ State='path'; Path=$coverPath } } }
        $rotationCache = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
        & $extension { param($payload,$cache,$signals) Scan-ArtworkPayload $payload $cache $signals 12 2000 } $rotationPayload $rotationCache $artSignals
        $firstRotation = & $extension { param($payload,$cache) Add-ArtworkToSnapshot $payload $cache } $rotationPayload $rotationCache
        & $extension { param($payload,$cache,$signals) Scan-ArtworkPayload $payload $cache $signals 12 2000 } $rotationPayload $rotationCache $artSignals
        $secondRotation = & $extension { param($payload,$cache) Add-ArtworkToSnapshot $payload $cache } $rotationPayload $rotationCache
        $firstGames = ($firstRotation.Lines[0] | ConvertFrom-Json).payload
        $secondGames = ($secondRotation.Lines[0] | ConvertFrom-Json).payload
        Assert-Equal $firstGames[0].artworkState 'revision' 'first scan did not visit first game'
        Assert-Equal $firstGames[1].artworkState 'unknown' 'unscanned game was not unknown'
        Assert-Equal $secondGames[0].artworkState 'revision' 'rotation discarded a cached image'
        Assert-Equal $secondGames[1].artworkState 'revision' 'rotation starved later game'
        $artCache['payload'] = $artPayload
        $artConnection = New-MockConnection ('a' * 32) $artCache
        $artConnection.Role = 'sunshine'
        $artConnection.Stream = [System.IO.MemoryStream]::new()
        $request = [pscustomobject]@{ type='command'; command='artwork_read'; requestId=('b' * 32); transferId=('d' * 32); id='60000000-0000-0000-0000-000000000001'; revision=$image.Revision; offset=0 }
        # Hold the dedicated scanner where an opener could stall. The control
        # handler must still serve already cached bytes without waiting for it.
        $gate = [System.Threading.ManualResetEventSlim]::new($false)
        $artStop = [System.Threading.CancellationTokenSource]::new()
        $scanQueue = [System.Collections.Concurrent.BlockingCollection[object]]::new(1)
        $artCache['artworkQueue'] = $scanQueue
        $artCache['artworkInstance'] = 'test-instance'
        $scanSpace = [System.Management.Automation.Runspaces.RunspaceFactory]::CreateRunspace()
        $scanSpace.Open()
        $scanWorker = [PowerShell]::Create(); $scanWorker.Runspace = $scanSpace
        [void]$scanWorker.AddScript('param($path,$cache,$signals,$stop,$gate) Import-Module -Name $path -Force; Start-ArtworkWorker $cache $signals $stop "test-instance" $gate').AddArgument($ModulePath).AddArgument($artCache).AddArgument($artSignals).AddArgument($artStop).AddArgument($gate)
        $scanHandle = $scanWorker.BeginInvoke()
        try {
            Assert-True ($scanQueue.TryAdd($artPayload)) 'scanner queue rejected work'
            $scanDeadline = [DateTime]::UtcNow.AddSeconds(3)
            while ($scanQueue.Count -gt 0 -and [DateTime]::UtcNow -lt $scanDeadline) { Start-Sleep -Milliseconds 10 }
            Assert-Equal $scanQueue.Count 0 'scanner did not take stalled job'
            $watch = [System.Diagnostics.Stopwatch]::StartNew()
            & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $request
            $watch.Stop()
            Assert-True ($watch.ElapsedMilliseconds -lt 1000) 'stalled artwork opener blocked control'
        } finally {
            $artStop.Cancel(); $gate.Set()
            [void]$scanHandle.AsyncWaitHandle.WaitOne(3000)
            if ($scanHandle.IsCompleted) { $scanWorker.Dispose(); $scanSpace.Dispose() }
            $gate.Dispose(); $artStop.Dispose(); $scanQueue.Dispose()
        }
        $reply = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$reply.success) 'pinned artwork read failed'
        Assert-Equal $reply.data ([Convert]::ToBase64String($coverBytes)) 'artwork chunk bytes'
        $artConnection.ArtworkPins.Clear()
        $largeBytes = [byte[]]::new(16777216)
        $largeBytes[16777215] = 42
        $artCache['artworkResult'] = @{ Generation=0L; Instance='test-instance'; Images=@{ '60000000-0000-0000-0000-000000000001'=@{ Revision=$image.Revision; MimeType='image/png'; Data=$largeBytes } }; Ready='large' }
        $request.offset = 0
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $request
        $largeFirst = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$largeFirst.success) 'large cached artwork pin failed'
        $request.offset = 16769024
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $request
        $largeReply = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$largeReply.success) 'large cached artwork final chunk failed'
        Assert-Equal $largeReply.total_bytes 16777216 'large artwork size changed'
        $largeChunk = [Convert]::FromBase64String($largeReply.data)
        Assert-Equal $largeChunk.Length 8192 'large artwork chunk was not bounded'
        Assert-Equal $largeChunk[8191] 42 'large artwork chunk did not use cached bytes'
        $request.offset = 0
        Assert-Equal $reply.total_bytes $coverBytes.Length 'artwork total size'
        $request.revision = '0' * 64
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $request
        $failedReply = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True (-not [bool]$failedReply.success) 'changed revision was accepted'
        $artConnection.ArtworkPins.Clear()
        $largeBytes = [byte[]]::new(8193)
        [Array]::Copy($coverBytes, $largeBytes, $coverBytes.Length)
        $sha = [System.Security.Cryptography.SHA256]::Create()
        try { $largeRevision = [BitConverter]::ToString($sha.ComputeHash($largeBytes)).Replace('-', '').ToLowerInvariant() }
        finally { $sha.Dispose() }
        $largeImage = @{ Data=$largeBytes; Revision=$largeRevision; MimeType='image/png' }
        $artCache['artworkResult'] = @{ Generation=0L; Instance=''; Images=@{ '60000000-0000-0000-0000-000000000001'=$largeImage }; Order=@('60000000-0000-0000-0000-000000000001'); Ready='large' }
        $request.revision = $largeRevision; $request.offset = 0
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $request
        $firstChunk = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$firstChunk.success) 'first chunk did not pin artwork'
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $request
        $secondReader = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$secondReader.success) 'second reader could not share the pin'
        $otherTransfer = [pscustomobject]@{ type='command'; command='artwork_read'; requestId=('e' * 32); transferId=('f' * 32); id=$request.id; revision=$largeRevision; offset=0 }
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $otherTransfer
        $otherReply = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$otherReply.success) 'independent transfer could not pin same image'
        Assert-Equal $artConnection.ArtworkPins.Count 2 'distinct transfers did not receive distinct pins'
        $release = [pscustomobject]@{ type='command'; command='artwork_release'; requestId=('e' * 32); transferId=$otherTransfer.transferId }
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $release
        $releaseReply = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$releaseReply.success) 'explicit release failed'
        Assert-Equal $artConnection.ArtworkPins.Count 1 'explicit release removed another transfer'
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $release
        $repeatRelease = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$repeatRelease.success) 'repeated release was not idempotent'
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $otherTransfer
        $releasedReplay = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True (-not [bool]$releasedReplay.success) 'released transfer recreated its pin'
        $beforeRead = [pscustomobject]@{ type='command'; command='artwork_release'; requestId=('e' * 32); transferId=('a' * 32) }
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $beforeRead
        $beforeReply = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$beforeReply.success) 'release before read was not accepted'
        $lateRead = [pscustomobject]@{ type='command'; command='artwork_read'; requestId=('e' * 32); transferId=$beforeRead.transferId; id=$request.id; revision=$largeRevision; offset=0 }
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $lateRead
        $lateReply = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True (-not [bool]$lateReply.success) 'late read recreated a released transfer'
        $conflicting = [pscustomobject]@{ type='command'; command='artwork_read'; requestId=('e' * 32); transferId=$request.transferId; id=$request.id; revision=('0' * 64); offset=0 }
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $conflicting
        $conflictReply = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True (-not [bool]$conflictReply.success) 'transfer token accepted a different revision'
        Assert-Equal $conflictReply.transferId $request.transferId 'failed response lost transfer correlation'
        for ($attempt = 0; $attempt -lt 20; $attempt++) {
            & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $request
            $retry = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
            Assert-True ([bool]$retry.success) 'repeated first chunk exhausted the pin'
        }
        $evictPayload = @{ Generation=0L; Artwork=@{ other=@{ State='path'; Path=$coverPath } } }
        & $extension { param($payload,$cache,$signals) Scan-ArtworkPayload $payload $cache $signals 12 2000 '' 12 1 } $evictPayload $artCache $artSignals
        Assert-True (-not $artCache['artworkResult'].Images.ContainsKey($request.id)) 'scan did not evict original image'
        $request.offset = 8192
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $request
        $lastChunk = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$lastChunk.success) 'eviction interrupted pinned artwork'
        Assert-Equal $lastChunk.data ([Convert]::ToBase64String($largeBytes, 8192, 1)) 'pinned final chunk differs'
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $request
        $secondLastChunk = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$secondLastChunk.success) 'first reader released another reader pin'
        Assert-Equal $artConnection.ArtworkPins.Count 1 'completed artwork pin did not survive retries'
        $pin = @($artConnection.ArtworkPins.Values)[0]
        & $extension { param($connection,$now) Remove-ExpiredArtworkPins $connection $now } $artConnection $pin.Expires.AddTicks(-1)
        Assert-Equal $artConnection.ArtworkPins.Count 1 'pin expired before its idle deadline'
        & $extension { param($connection,$now) Remove-ExpiredArtworkPins $connection $now } $artConnection $pin.Expires.AddSeconds(1)
        Assert-Equal $artConnection.ArtworkPins.Count 0 'idle pin was not expired by periodic pruning'
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $artConnection $request
        $expiredReply = ($artConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True (-not [bool]$expiredReply.success) 'expired evicted pin remained readable'
        # No automatic snapshot or scan is needed to recover an evicted image.
        $demandCache = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
        $demandCache['payload'] = $artPayload
        $demandCache['artworkInstance'] = 'demand-instance'
        $demandQueue = [System.Collections.Concurrent.BlockingCollection[object]]::new(1)
        $demandCache['artworkPriorityQueue'] = $demandQueue
        $demandConnection = New-MockConnection ('a' * 32) $demandCache
        $demandConnection.Role = 'sunshine'; $demandConnection.Stream = [System.IO.MemoryStream]::new()
        $demandRequest = [pscustomobject]@{ type='command'; command='artwork_read'; requestId=('1' * 32); transferId=('2' * 32); id='60000000-0000-0000-0000-000000000001'; revision=$image.Revision; offset=0 }
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        $demandPending = ($demandConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-Equal $demandPending.error 'artwork_pending' 'cache miss did not schedule artwork scan'
        $scanJob = $null
        Assert-True ($demandQueue.TryTake([ref]$scanJob)) 'priority scan job missing'
        & $extension { param($job,$cache,$signals) Invoke-ArtworkScanJob $job $cache $signals 'demand-instance' } $scanJob $demandCache $demandConnection.Signals
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        $demandReply = ($demandConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$demandReply.success) 'prioritized rescan did not restore evicted artwork'
        $demandConnection.ArtworkPins.Clear()
        # A persisted unknown revision cannot trust a matching cache entry.
        # Change the file after caching and force a fresh hash for this token.
        $demandRequest | Add-Member -NotePropertyName refresh -NotePropertyValue $true
        $demandRequest.transferId = '6' * 32
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        Assert-Equal (($demandConnection.Queue.Take())[0] | ConvertFrom-Json).error 'artwork_pending' 'refresh did not verify cached bytes'
        $verifiedJob = $null
        Assert-True ($demandQueue.TryTake([ref]$verifiedJob)) 'verification job missing'
        & $extension { param($job,$cache,$signals) Invoke-ArtworkScanJob $job $cache $signals 'demand-instance' } $verifiedJob $demandCache $demandConnection.Signals
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        $verifiedReply = ($demandConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$verifiedReply.success) 'fresh matching hash was rejected'
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        $pinnedReply = ($demandConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$pinnedReply.success) 'pinned retry incorrectly restarted verification'
        Assert-Equal $demandQueue.Count 0 'pinned retry enqueued another scan'
        $demandConnection.ArtworkPins.Clear()
        $demandRequest.transferId = '5' * 32
        $changedBytes = [byte[]]$coverBytes.Clone(); $changedBytes[11] = 99
        [System.IO.File]::WriteAllBytes($coverPath, $changedBytes)
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        $refreshPending = ($demandConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-Equal $refreshPending.error 'artwork_pending' 'refresh served stale cached bytes'
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        $refreshRetry = ($demandConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-Equal $refreshRetry.error 'artwork_pending' 'refresh retry bypassed the pending scan'
        $refreshJob = $null
        Assert-True ($demandQueue.TryTake([ref]$refreshJob)) 'refresh scan job missing'
        & $extension { param($job,$cache,$signals) Invoke-ArtworkScanJob $job $cache $signals 'demand-instance' } $refreshJob $demandCache $demandConnection.Signals
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        $changedReply = ($demandConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-Equal $changedReply.error 'artwork_unavailable' 'changed artwork passed forced revision verification'
        [System.IO.File]::WriteAllBytes($coverPath, $coverBytes)
        $demandConnection.ArtworkDemands.Clear()
        $demandCache['artworkResult'] = @{ Generation=0L; Instance='demand-instance'; Images=@{ '60000000-0000-0000-0000-000000000001'=$image }; Order=@('60000000-0000-0000-0000-000000000001'); Ready='stale' }
        $missingPath = Join-Path ([System.IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString('N'))
        $failedPayload = @{ Generation=0L; Artwork=@{ '60000000-0000-0000-0000-000000000001'=@{ State='path'; Path=$missingPath } } }
        $demandCache['payload'] = $failedPayload
        $demandRequest.transferId = '3' * 32
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        Assert-Equal (($demandConnection.Queue.Take())[0] | ConvertFrom-Json).error 'artwork_pending' 'missing cover was not queued'
        $scanJob = $null
        Assert-True ($demandQueue.TryTake([ref]$scanJob)) 'failed scan job missing'
        & $extension { param($job,$cache,$signals) Invoke-ArtworkScanJob $job $cache $signals 'demand-instance' } $scanJob $demandCache $demandConnection.Signals
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        Assert-Equal (($demandConnection.Queue.Take())[0] | ConvertFrom-Json).error 'artwork_unavailable' 'failed scan did not terminate retry'
        $demandCache['payload'] = $artPayload
        $demandRequest.refresh = $false
        $demandCache['artworkResult'] = @{ Generation=0L; Instance='demand-instance'; Images=@{}; Order=@(); Ready='none' }
        $demandRequest.transferId = '4' * 32
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        Assert-Equal (($demandConnection.Queue.Take())[0] | ConvertFrom-Json).error 'artwork_pending' 'release race scan was not queued'
        $scanJob = $null
        Assert-True ($demandQueue.TryTake([ref]$scanJob)) 'release race job missing'
        $demandRelease = [pscustomobject]@{ type='command'; command='artwork_release'; requestId=('1' * 32); transferId=$demandRequest.transferId }
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRelease
        $releasedDemand = ($demandConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True ([bool]$releasedDemand.success) 'release during scan failed'
        & $extension { param($job,$cache,$signals) Invoke-ArtworkScanJob $job $cache $signals 'demand-instance' } $scanJob $demandCache $demandConnection.Signals
        & $extension { param($connection,$message) Invoke-ExtensionCommand $connection $message } $demandConnection $demandRequest
        $lateDemand = ($demandConnection.Queue.Take())[0] | ConvertFrom-Json
        Assert-True (-not [bool]$lateDemand.success) 'completed scan revived released transfer'
        $demandQueue.Dispose(); $demandConnection.Queue.Dispose(); $demandConnection.Stream.Dispose()
        $overflow = New-MockConnection ('a' * 32) $artCache
        $overflow.Role = 'sunshine'; $overflow.Stream = [System.IO.MemoryStream]::new()
        $overflow.ArtworkTombstones = @{}
        $fakeNow = [DateTime]'2040-01-01T00:00:00Z'
        for ($i = 0; $i -lt 4096; $i++) { $overflow.ArtworkTombstones[('{0:x32}' -f $i)] = $fakeNow.AddSeconds(60) }
        $overflowRelease = [pscustomobject]@{ transferId=('f' * 32) }
        & $extension { param($connection,$message,$now) Release-ArtworkTransfer $connection $message ('e' * 32) $now } $overflow $overflowRelease $fakeNow
        $overflowReply = ($overflow.Queue.Take())[0] | ConvertFrom-Json
        Assert-True (-not [bool]$overflowReply.success) 'full tombstone table acknowledged an unfenced release'
        Assert-True ($null -ne $overflow.ArtworkReleaseOverflowUntil) 'full table did not fail closed'
        & $extension { param($connection,$now) Remove-ExpiredArtworkPins $connection $now } $overflow $fakeNow.AddSeconds(60)
        Assert-Equal $overflow.ArtworkTombstones.Count 0 'expired tombstones were retained'
        Assert-True (-not $overflow.ArtworkReleaseOverflowUntil) 'overflow fence did not expire'
        $overflow.Queue.Dispose(); $overflow.Stream.Dispose()
        $artConnection.Queue.Dispose()
        $artConnection.Stream.Dispose()
        [System.IO.File]::WriteAllBytes($coverPath, [byte[]](1,2,3,4,5,6,7,8,9,10,11,12))
        $invalid = & $extension { param($p) $budget = 100L; Read-ArtworkFile $p 2000 ([ref]$budget) } $coverPath
        Assert-True ($null -eq $invalid) 'invalid image signature accepted'
    } finally { Remove-Item -LiteralPath $coverPath -ErrorAction SilentlyContinue }

    $eventSignals = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
    $timer = [System.Timers.Timer]::new(20)
    $timer.AutoReset = $false
    $eventSubscription = & $extension {
        param($source,$signals)
        Register-LibraryChangeEvent $source 'Elapsed' $signals
    } $timer $eventSignals
    Assert-True ($eventSubscription.SubscriptionId -gt 0) 'event registration did not return a removable subscriber'
    try {
        $timer.Start()
        $deadline = [DateTime]::UtcNow.AddSeconds(2)
        while (-not $eventSignals.ContainsKey('generation') -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 20 }
        Assert-True ([long]$eventSignals['generation'] -gt 0) 'installed module event action did not mark the shared library generation'
    } finally {
        $timer.Stop()
        Unregister-Event -SubscriptionId $eventSubscription.SubscriptionId -ErrorAction SilentlyContinue
        $timer.Dispose()
    }

    if (-not ('BlockedPlayniteWriteStream' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
public sealed class BlockedPlayniteWriteStream : Stream {
    readonly TaskCompletionSource<object> pending = new TaskCompletionSource<object>();
    public bool WasDisposed { get; private set; }
    public override bool CanRead => false;
    public override bool CanSeek => false;
    public override bool CanWrite => !WasDisposed;
    public override long Length => throw new NotSupportedException();
    public override long Position { get => throw new NotSupportedException(); set => throw new NotSupportedException(); }
    public override void Flush() { }
    public override int Read(byte[] buffer, int offset, int count) => throw new NotSupportedException();
    public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
    public override void SetLength(long value) => throw new NotSupportedException();
    public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
    public override Task WriteAsync(byte[] buffer, int offset, int count, CancellationToken token) => pending.Task;
    public override ValueTask WriteAsync(ReadOnlyMemory<byte> buffer, CancellationToken token = default) => new ValueTask(pending.Task);
    protected override void Dispose(bool disposing) { WasDisposed = true; pending.TrySetCanceled(); base.Dispose(disposing); }
}
'@
    }
    $blockedStream = [BlockedPlayniteWriteStream]::new()
    $blockedWriter = [System.IO.StreamWriter]::new($blockedStream, [System.Text.UTF8Encoding]::new($false), 1024, $true)
    $blockedWriter.AutoFlush = $true
    $blockedConnection = New-MockConnection ('3' * 32) ([System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new())
    $blockedConnection.Stream = $blockedStream
    $blockedFrame = @{ Lines=[string[]]@('{"type":"snapshotComplete"}'); CapturedAt=[DateTime]::UtcNow; Generation=0L; RequestId='' }
    try {
        Assert-Throws { & $extension { param($connection,$frame,$writer) Write-QueuedFrame $connection $frame $writer } $blockedConnection $blockedFrame $blockedWriter } 'blocked completion write did not time out'
        Assert-True $blockedStream.WasDisposed 'timed out completion left its pipe open'
    } finally { $blockedWriter.Dispose(); $blockedStream.Dispose(); $blockedConnection.Queue.Dispose() }

    Assert-Throws {
        & $extension { param($api) Capture-SnapshotPayload $api } (New-MockApi $false) | Out-Null
    } 'capture was accepted outside the UI dispatcher'

    $api = New-MockApi $true
    $payload = & $extension { param($api) Capture-SnapshotPayload $api } $api
    Assert-Equal $payload.Games 1 'capture game count'
    Assert-Equal $payload.Lines.Count 3 'capture payload frame count'
    $plugins = $payload.Lines[0] | ConvertFrom-Json
    $categories = $payload.Lines[1] | ConvertFrom-Json
    $games = $payload.Lines[2] | ConvertFrom-Json
    Assert-Equal $plugins.type 'plugins' 'plugin frame type'
    Assert-Equal $categories.type 'categories' 'category frame type'
    Assert-Equal $games.type 'games' 'game frame type'
    Assert-Equal $games.payload[0].name 'Mock Game' 'captured game name'
    Assert-Equal $games.payload[0].categories[0] 'Favorites' 'captured category'
    Assert-True ([bool]$games.payload[0].installed) 'installed flag missing'

    $cache = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
    Assert-True ($cache.TryAdd('payload', [object]$payload)) 'snapshot cache setup failed'
    $old = New-MockConnection ('a' * 32) $cache
    $new = New-MockConnection ('b' * 32) $cache
    try {
        $api.Database.Games[0].IsRunning = $true
        $api.Database.Games[0].InstallDirectory = $extensionDirectory
        $api.Database.Games[0].GameActions = @([pscustomobject]@{ IsPlayAction = $true; Path = $ModulePath })
        foreach ($connection in @($old, $new)) {
            $connection.Role = 'launcher'
            $connection.Mode = 'fullscreen'
            $connection.Api = $api
            $connection.Dispatcher = $api.MainView.UIDispatcher
            $connection.Stream = [System.IO.MemoryStream]::new()
        }
        & $extension { param($connection) Replay-RunningGame $connection } $old
        & $extension { param($connection) Replay-RunningGame $connection } $new
        Assert-Equal (($old.Queue.Take())[0] | ConvertFrom-Json).status.id '60000000-0000-0000-0000-000000000001' 'active game missing on first connection'
        Assert-Equal (($new.Queue.Take())[0] | ConvertFrom-Json).status.name 'gameStarted' 'active game missing on reconnect'
        $api.Database.Games[0].IsRunning = $false
        & $extension { param($connection) Replay-RunningGame $connection } $new
        Assert-Equal $new.Queue.Count 0 'stopped game was replayed after reconnect'
        $api.Database.Games[0].IsRunning = $true
        $api.Database.Games += [pscustomobject]@{ Id='60000000-0000-0000-0000-000000000002'; IsRunning=$true; InstallDirectory='C:\Games\Other'; GameActions=@() }
        & $extension { param($connection) Replay-RunningGame $connection } $new
        Assert-Equal $new.Queue.Count 0 'ambiguous running games emitted a fullscreen status'
        $api.Database.Games = @($api.Database.Games[0])
        $api.Database.Games[0].Id = 'not-a-guid'
        & $extension { param($connection) Replay-RunningGame $connection } $new
        Assert-Equal $new.Queue.Count 0 'malformed running game emitted a fullscreen status'
        $api.Database.Games[0].Id = '60000000-0000-0000-0000-000000000001'
        $api.Database.Games[0].IsRunning = $false
        $requestId = 'c' * 32
        $oldFrame = @(& $extension { param($connection, $id) New-SnapshotFrame $connection $id } $old $requestId)
        Assert-Equal $oldFrame.Count 5 'snapshot bracket length'
        $oldStart = $oldFrame[0] | ConvertFrom-Json
        $oldEnd = $oldFrame[-1] | ConvertFrom-Json
        Assert-Equal $oldStart.type 'snapshotStart' 'start marker'
        Assert-Equal $oldEnd.type 'snapshotComplete' 'end marker'
        Assert-Equal $oldStart.nonce ('a' * 32) 'old start nonce'
        Assert-Equal $oldEnd.nonce ('a' * 32) 'old end nonce'
        Assert-Equal $oldStart.requestId $requestId 'start request ID'
        Assert-Equal $oldEnd.requestId $requestId 'end request ID'
        Assert-Equal $oldEnd.games 1 'complete game count'
        Assert-True (& $extension { param($connection, $frame) Add-Frame $connection $frame } $old $oldFrame) 'old frame was not queued'
        $old.Role = 'sunshine'
        & $extension { param($connection) Invoke-ExtensionCommand $connection ([pscustomobject]@{ type='command'; command='snapshot'; requestId='bad-id' }) } $old
        Assert-Equal $old.Queue.Count 1 'malformed snapshot request ID was queued'

        $new.Signals['dirty'] = $payload.CapturedAt.AddSeconds(1)
        Assert-Throws {
            & $extension { param($connection) New-SnapshotFrame $connection } $new | Out-Null
        } 'stale cached snapshot was accepted after a library change'
        $new.Signals.Clear()

        $api.Database.Games[0].Name = 'Updated Game'
        $api.Database.Games[0].IsInstalled = $false
        $updatedPayload = & $extension { param($source) Capture-SnapshotPayload $source } $api
        $cache['payload'] = [object]$updatedPayload
        $newFrame = @(& $extension { param($connection) New-SnapshotFrame $connection } $new)
        $oldGame = $oldFrame[3] | ConvertFrom-Json
        $newGame = $newFrame[3] | ConvertFrom-Json
        Assert-Equal $oldGame.payload[0].name 'Mock Game' 'queued old frame changed after recapture'
        Assert-True ([bool]$oldGame.payload[0].installed) 'queued old installed state changed'
        Assert-Equal $newGame.payload[0].name 'Updated Game' 'fresh frame missed library change'
        Assert-True (-not [bool]$newGame.payload[0].installed) 'fresh frame missed uninstall state'
        Assert-True (& $extension { param($connection, $frame) Add-Frame $connection $frame } $new $newFrame) 'new frame was not queued'
        Assert-Equal $old.Queue.Count 1 'old outbox was changed by reconnect'
        Assert-Equal $new.Queue.Count 1 'old frame leaked into new outbox'
        Assert-Equal (($old.Queue.Take())[0] | ConvertFrom-Json).nonce ('a' * 32) 'old queued frame was restamped'
        Assert-Equal (($new.Queue.Take())[0] | ConvertFrom-Json).nonce ('b' * 32) 'new queued frame used stale nonce'
        Assert-Equal ($newFrame[-1] | ConvertFrom-Json).nonce ('b' * 32) 'new end nonce'
        $new.Api = $api
        $new.Dispatcher = $api.MainView.UIDispatcher
        $new.Stream = [System.IO.MemoryStream]::new()
        & $extension { param($connection,$id) Queue-Snapshot $connection $id } $new $requestId
        $queuedBeforeChange = $null
        Assert-True $new.Queue.TryTake([ref]$queuedBeforeChange, 2000) 'initial requested snapshot was not queued'
        Assert-Equal $queuedBeforeChange.CapturedAt $updatedPayload.CapturedAt 'queued snapshot lost capture generation'
        Assert-Equal $queuedBeforeChange.RequestId $requestId 'queued snapshot lost request correlation'
        $staleWriter = [System.IO.StringWriter]::new()
        $new.Signals['dirty'] = $updatedPayload.CapturedAt.AddTicks(1)
        Assert-True (-not (& $extension { param($connection,$frame,$writer) Write-QueuedFrame $connection $frame $writer } $new $queuedBeforeChange $staleWriter)) 'stale queued snapshot was sent'
        Assert-Equal $staleWriter.ToString() '' 'stale queued snapshot emitted a partial bracket'
        & $extension { param($connection,$id) Queue-Snapshot $connection $id } $new $queuedBeforeChange.RequestId
        $recoveredFrame = $null
        Assert-True $new.Queue.TryTake([ref]$recoveredFrame, 2000) 'stale requested snapshot was not recaptured'
        Assert-True ($recoveredFrame.CapturedAt -gt $queuedBeforeChange.CapturedAt) 'stale requested snapshot was not recaptured'
        Assert-Equal $recoveredFrame.RequestId $requestId 'recaptured snapshot lost request correlation'
        $interruptWriter = [pscustomobject]@{
            Lines = [System.Collections.Generic.List[string]]::new()
            Signals = $new.Signals
            CapturedAt = $recoveredFrame.CapturedAt
        }
        $interruptWriter | Add-Member -MemberType ScriptMethod -Name WriteLine -Value {
            param([string]$line)
            $this.Lines.Add($line)
            if ($this.Lines.Count -eq 1) { $this.Signals['dirty'] = $this.CapturedAt.AddTicks(1) }
        }
        Assert-True (-not (& $extension { param($connection,$frame,$writer) Write-QueuedFrame $connection $frame $writer } $new $recoveredFrame $interruptWriter)) 'mid-frame library change completed a stale snapshot'
        Assert-Equal $interruptWriter.Lines.Count 1 'mid-frame change emitted payload or completion'
        Assert-Equal ($interruptWriter.Lines[0] | ConvertFrom-Json).type 'snapshotStart' 'interrupted frame lost its start marker'
        $api.Database.Games[0].Name = 'Latest Game'
        & $extension { param($connection,$id) Queue-Snapshot $connection $id } $new $recoveredFrame.RequestId
        $afterInterrupt = $null
        Assert-True $new.Queue.TryTake([ref]$afterInterrupt, 2000) 'mid-frame requested snapshot was not recaptured'
        Assert-Equal $afterInterrupt.RequestId $requestId 'mid-frame recapture lost request correlation'
        Assert-Equal ($afterInterrupt.Lines[0] | ConvertFrom-Json).nonce ('b' * 32) 'mid-frame recapture changed connection nonce'
        Assert-Equal ($afterInterrupt.Lines[-1] | ConvertFrom-Json).requestId $requestId 'mid-frame recapture changed completion request ID'
        Assert-Equal ($afterInterrupt.Lines[3] | ConvertFrom-Json).payload[0].name 'Latest Game' 'mid-frame recapture reused stale library data'
        $afterInterrupt.BeforeComplete = {
            & $extension { param($signals) Mark-LibraryDirty $signals } $new.Signals
        }.GetNewClosure()
        $finalBoundaryWriter = [System.IO.StringWriter]::new()
        Assert-True (-not (& $extension { param($connection,$frame,$writer) Write-QueuedFrame $connection $frame $writer } $new $afterInterrupt $finalBoundaryWriter)) 'final-line dirty signal completed a stale snapshot'
        $boundaryLines = @($finalBoundaryWriter.ToString().Trim().Split("`n"))
        Assert-Equal $boundaryLines.Count 4 'final-line dirty signal emitted snapshotComplete'
        Assert-Equal ($boundaryLines[0] | ConvertFrom-Json).nonce ('b' * 32) 'partial bracket changed nonce'
        $api.Database.Games[0].Name = 'Newest Game'
        & $extension { param($connection,$id) Queue-Snapshot $connection $id } $new $afterInterrupt.RequestId
        $finalRecapture = $null
        Assert-True $new.Queue.TryTake([ref]$finalRecapture, 2000) 'final-line requested snapshot was not recaptured'
        Assert-Equal $finalRecapture.RequestId $requestId 'final-line recapture changed request ID'
        Assert-Equal ($finalRecapture.Lines[0] | ConvertFrom-Json).nonce ('b' * 32) 'final-line recapture changed nonce'
        Assert-Equal ($finalRecapture.Lines[-1] | ConvertFrom-Json).nonce ('b' * 32) 'final-line recapture changed completion nonce'
        Assert-Equal ($finalRecapture.Lines[-1] | ConvertFrom-Json).requestId $requestId 'final-line recapture changed completion request ID'
        Assert-Equal ($finalRecapture.Lines[3] | ConvertFrom-Json).payload[0].name 'Newest Game' 'final-line recapture retained stale game'
        $finalBoundaryWriter.Dispose()
        $new.Signals.Clear()
        $freshWriter = [System.IO.StringWriter]::new()
        Assert-True (& $extension { param($connection,$frame,$writer) Write-QueuedFrame $connection $frame $writer } $new $queuedBeforeChange $freshWriter) 'fresh snapshot was dropped'
        Assert-Equal @($freshWriter.ToString().Trim().Split("`n")).Count 5 'complete queued snapshot was not written'
        Assert-Equal (($freshWriter.ToString().Trim().Split("`n")[-1]) | ConvertFrom-Json).nonce ('b' * 32) 'queued completion nonce changed'
        Assert-Equal (($freshWriter.ToString().Trim().Split("`n")[-1]) | ConvertFrom-Json).requestId $requestId 'queued completion request ID changed'
        $staleWriter.Dispose(); $freshWriter.Dispose()
        Assert-Throws {
            & $extension { param($connection) New-SnapshotFrame $connection } (New-MockConnection ('d' * 32) ([System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new())) | Out-Null
        } 'frame built without a complete cached payload'

        $tooManyLines = [object[]]::new(513)
        Assert-True (-not (& $extension { param($connection, $frame) Add-Frame $connection $frame } $new $tooManyLines)) 'oversized frame count was accepted'
        Assert-True (-not (& $extension { param($connection, $frame) Add-Frame $connection $frame } $new @('x' * 8388608))) 'oversized frame bytes were accepted'
        Assert-Throws {
            & $extension { Convert-FrameLine @{ type='status'; data=('x' * 1048576) } } | Out-Null
        } 'oversized JSON line was accepted'
        for ($index = 0; $index -lt 8; $index++) {
            Assert-True (& $extension { param($connection) Add-Frame $connection @('ok') } $new) 'bounded queue filled early'
        }
        Assert-True (-not (& $extension { param($connection) Add-Frame $connection @('overflow') } $new)) 'full queue accepted a partial frame'
        Assert-Equal $new.Queue.Count 8 'queue bound changed'
    } finally {
        $old.Stream.Dispose()
        $new.Stream.Dispose()
        $old.Queue.Dispose()
        $new.Queue.Dispose()
    }

    Assert-Throws {
        & $extension { Assert-Hello $null ([pscustomobject]@{ type='command'; pid=1 }) 1 }
    } 'untyped hello was accepted'
    $helloBytes = [System.Text.Encoding]::UTF8.GetBytes('{"type":"hello"}' + "`n")
    $helloStream = [System.IO.MemoryStream]::new($helloBytes)
    try {
        Assert-Equal (& $extension { param($stream) Read-BoundedHello $stream } $helloStream) '{"type":"hello"}' 'bounded hello line'
    } finally { $helloStream.Dispose() }
    $oversizedHello = [System.IO.MemoryStream]::new([System.Text.Encoding]::UTF8.GetBytes('x' * 8192))
    try {
        Assert-Throws {
            & $extension { param($stream) Read-BoundedHello $stream } $oversizedHello | Out-Null
        } 'oversized hello was accepted'
    } finally { $oversizedHello.Dispose() }
    $path = 'C:\Program Files\Amaterasu\amaterasu.exe'
    & $extension { param($p) Assert-SunshineIdentity 120 100 100 $p $p 'amaterasu worker-user-serve' } $path
    Assert-Throws {
        & $extension { param($p) Assert-SunshineIdentity 120 100 99 $p $p 'amaterasu worker-user-serve' } $path
    } 'wrong parent PID was accepted'
    Assert-Throws {
        & $extension { param($p) Assert-SunshineIdentity 120 100 100 $p 'C:\Other\amaterasu.exe' 'amaterasu worker-user-serve' } $path
    } 'wrong service image was accepted'
    Assert-Throws {
        & $extension { param($p) Assert-SunshineIdentity 120 100 100 $p $p 'amaterasu background' } $path
    } 'noninteractive process was accepted'
    Assert-Throws {
        & $extension { Assert-SunshineIdentity 120 100 100 'relative.exe' 'relative.exe' 'amaterasu worker-user-serve' }
    } 'relative image path was accepted'

    $connections = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
    $fullscreen = New-MockConnection ('e' * 32) $cache
    $fullscreen.Role = 'launcher'; $fullscreen.Mode = 'fullscreen'; $fullscreen.Stream = [System.IO.MemoryStream]::new()
    $fullscreen.ShutdownRequested = [System.Threading.ManualResetEventSlim]::new($false)
    $fullscreen.ShutdownWritten = [System.Threading.ManualResetEventSlim]::new($false)
    $desktop = New-MockConnection ('f' * 32) $cache
    $desktop.Role = 'launcher'; $desktop.Mode = ''; $desktop.Stream = [System.IO.MemoryStream]::new()
    $desktop.ShutdownRequested = [System.Threading.ManualResetEventSlim]::new($false)
    $desktop.ShutdownWritten = [System.Threading.ManualResetEventSlim]::new($false)
    $sunshine = New-MockConnection ('2' * 32) $cache
    $sunshine.Role = 'sunshine'; $sunshine.Mode = ''; $sunshine.Stream = [System.IO.MemoryStream]::new()
    $sunshine.ShutdownRequested = [System.Threading.ManualResetEventSlim]::new($false)
    $sunshine.ShutdownWritten = [System.Threading.ManualResetEventSlim]::new($false)
    Assert-True $connections.TryAdd('fullscreen', $fullscreen) 'fullscreen shutdown setup failed'
    Assert-True $connections.TryAdd('desktop', $desktop) 'desktop shutdown setup failed'
    Assert-True $connections.TryAdd('sunshine', $sunshine) 'sunshine shutdown setup failed'
    Assert-True (& $extension { param($connection) Add-Frame $connection @('queued-before-shutdown') } $fullscreen) 'shutdown queue setup failed'

    $shutdownApi = New-MockApi $true
    $shutdownGame = $shutdownApi.Database.Games[0]
    $shutdownGame.InstallDirectory = $extensionDirectory
    $shutdownGame.GameActions = @([pscustomobject]@{ IsPlayAction = $true; Path = $ModulePath })
    $shutdownGame.IsRunning = $true
    $activeShutdownLines = @(& $extension { param($api) Get-FullscreenShutdownLines $api $api.MainView.UIDispatcher } $shutdownApi)
    Assert-Equal $activeShutdownLines.Count 1 'single active game shutdown handoff line count'
    Assert-Equal ($activeShutdownLines[0] | ConvertFrom-Json).status.name 'gameStarted' 'single active game shutdown handoff status'
    Assert-Equal ($activeShutdownLines[0] | ConvertFrom-Json).status.id $shutdownGame.Id 'single active game shutdown handoff ID'

    $ambiguousShutdownApi = New-MockApi $true
    $ambiguousShutdownGame = $ambiguousShutdownApi.Database.Games[0]
    $ambiguousShutdownGame.InstallDirectory = $extensionDirectory
    $ambiguousShutdownGame.GameActions = @([pscustomobject]@{ IsPlayAction = $true; Path = $ModulePath })
    $ambiguousShutdownGame.IsRunning = $true
    $ambiguousShutdownApi.Database.Games += [pscustomobject]@{
        Id = '60000000-0000-0000-0000-000000000002'
        InstallDirectory = $extensionDirectory
        GameActions = @([pscustomobject]@{ IsPlayAction = $true; Path = $ModulePath })
        IsRunning = $true
    }
    Assert-True ($null -eq (& $extension { param($api) Get-FullscreenShutdownLines $api $api.MainView.UIDispatcher } $ambiguousShutdownApi)) 'multiple running games emitted a shutdown status'
    Assert-Equal (@(& $extension { param($items,$api) Request-FullscreenShutdown $items $api $api.MainView.UIDispatcher } $connections $ambiguousShutdownApi).Count) 0 'ambiguous shutdown woke fullscreen writers'
    Assert-True (-not $fullscreen.ShutdownRequested.IsSet) 'ambiguous shutdown requested a status write'

    $idleShutdownApi = New-MockApi $true
    $idleShutdownLines = @(& $extension { param($api) Get-FullscreenShutdownLines $api $api.MainView.UIDispatcher } $idleShutdownApi)
    Assert-Equal $idleShutdownLines.Count 1 'idle shutdown line count'
    Assert-Equal ($idleShutdownLines[0] | ConvertFrom-Json).status.name 'playniteExiting' 'idle shutdown status'
    $unknownShutdownApi = New-MockApi $true
    $unknownShutdownApi.MainView.UIDispatcher = [FailingDispatcher]::new()
    Assert-True ($null -eq (& $extension { param($api) Get-FullscreenShutdownLines $api $api.MainView.UIDispatcher } $unknownShutdownApi)) 'failed running-game query emitted a shutdown status'
    $incompleteShutdownApi = New-MockApi $true
    $incompleteShutdownApi.Database.Games[0].IsRunning = $true
    Assert-True ($null -eq (& $extension { param($api) Get-FullscreenShutdownLines $api $api.MainView.UIDispatcher } $incompleteShutdownApi)) 'incomplete active-game data emitted a shutdown status'
    $rootInstallShutdownApi = New-MockApi $true
    $rootInstallShutdownApi.Database.Games[0].InstallDirectory = [System.IO.Path]::GetPathRoot($extensionDirectory)
    $rootInstallShutdownApi.Database.Games[0].GameActions = @([pscustomobject]@{ IsPlayAction = $true; Path = $ModulePath })
    $rootInstallShutdownApi.Database.Games[0].IsRunning = $true
    Assert-True ($null -eq (& $extension { param($api) Get-FullscreenShutdownLines $api $api.MainView.UIDispatcher } $rootInstallShutdownApi)) 'filesystem-root install directory emitted a shutdown status'
    $invalidIdShutdownApi = New-MockApi $true
    $invalidIdShutdownApi.Database.Games[0].Id = 'not-a-guid'
    $invalidIdShutdownApi.Database.Games[0].InstallDirectory = $extensionDirectory
    $invalidIdShutdownApi.Database.Games[0].GameActions = @([pscustomobject]@{ IsPlayAction = $true; Path = $ModulePath })
    $invalidIdShutdownApi.Database.Games[0].IsRunning = $true
    Assert-True ($null -eq (& $extension { param($api) Get-FullscreenShutdownLines $api $api.MainView.UIDispatcher } $invalidIdShutdownApi)) 'malformed game ID emitted a shutdown status'

    $writer = [System.IO.StringWriter]::new()
    $writerWorker = [PowerShell]::Create()
    try {
        [void]$writerWorker.AddScript('param($path,$connection,$writer) Import-Module -Name $path -Force; [void]$connection.ShutdownRequested.Wait(2000); $module = Get-Module AmaterasuPlaynite; & $module { param($c,$w) Write-FullscreenShutdown $c $w } $connection $writer').AddArgument($ModulePath).AddArgument($fullscreen).AddArgument($writer)
        $writerHandle = $writerWorker.BeginInvoke()
        & $extension { param($items,$api) $script:Connections = $items; $script:PlayniteApi = $api; $script:Stop = [System.Threading.CancellationTokenSource]::new(); $script:Subscriptions = @(); $script:Background = $null; OnApplicationStopped } $connections $shutdownApi
        [void]$writerWorker.EndInvoke($writerHandle)
        Assert-True $fullscreen.ShutdownWritten.IsSet 'shutdown did not wait for writer confirmation'
        $shutdownLines = @($writer.ToString().Trim().Split("`n") | Where-Object { $_ })
        Assert-Equal $shutdownLines.Count 1 'fullscreen shutdown handoff line count'
        Assert-Equal ($shutdownLines[0] | ConvertFrom-Json).status.name 'gameStarted' 'fullscreen shutdown did not preserve the active game'
        Assert-Equal ($shutdownLines[0] | ConvertFrom-Json).status.id $shutdownGame.Id 'fullscreen shutdown changed the active game ID'
        Assert-Equal $fullscreen.Queue.Count 1 'shutdown did not bypass queued frames'
        Assert-True (-not $desktop.ShutdownRequested.IsSet) 'desktop launcher received fullscreen shutdown'
        Assert-True (-not $sunshine.ShutdownRequested.IsSet) 'sunshine received fullscreen shutdown'
        Assert-True (-not $fullscreen.Stream.CanRead) 'fullscreen stream was not closed after confirmation'
        Assert-True (-not $desktop.Stream.CanRead) 'desktop stream was not closed'
        Assert-True (-not $sunshine.Stream.CanRead) 'sunshine stream was not closed'
    } finally { $writerWorker.Dispose(); $writer.Dispose(); $fullscreen.Queue.Dispose(); $desktop.Queue.Dispose(); $sunshine.Queue.Dispose() }

    $dead = New-MockConnection ('1' * 32) $cache
    $dead.Role = 'launcher'; $dead.Mode = 'fullscreen'; $dead.Stream = [System.IO.MemoryStream]::new()
    $dead.ShutdownRequested = [System.Threading.ManualResetEventSlim]::new($false)
    $dead.ShutdownWritten = [System.Threading.ManualResetEventSlim]::new($false)
    $deadConnections = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
    Assert-True $deadConnections.TryAdd('dead', $dead) 'dead writer setup failed'
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    & $extension { param($items,$api) $script:Connections = $items; $script:PlayniteApi = $api; $script:Stop = [System.Threading.CancellationTokenSource]::new(); $script:Subscriptions = @(); $script:Background = $null; OnApplicationStopped } $deadConnections $idleShutdownApi
    $timer.Stop()
    Assert-True ($timer.ElapsedMilliseconds -lt 1500) 'dead writer blocked Playnite shutdown'
    Assert-True $dead.ShutdownRequested.IsSet 'dead writer did not receive shutdown request'
    Assert-True (-not $dead.ShutdownWritten.IsSet) 'dead writer falsely confirmed a write'
    Assert-True (-not $dead.Stream.CanRead) 'dead writer pipe was not closed'
    $dead.Queue.Dispose()

    $unknown = New-MockConnection ('4' * 32) $cache
    $unknown.Role = 'launcher'; $unknown.Mode = 'fullscreen'; $unknown.Stream = [System.IO.MemoryStream]::new()
    $unknown.ShutdownRequested = [System.Threading.ManualResetEventSlim]::new($false)
    $unknown.ShutdownWritten = [System.Threading.ManualResetEventSlim]::new($false)
    $unknownConnections = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
    Assert-True $unknownConnections.TryAdd('unknown', $unknown) 'unknown shutdown setup failed'
    & $extension { param($items,$api) $script:Connections = $items; $script:PlayniteApi = $api; $script:Stop = [System.Threading.CancellationTokenSource]::new(); $script:Subscriptions = @(); $script:Background = $null; OnApplicationStopped } $unknownConnections $unknownShutdownApi
    Assert-True (-not $unknown.ShutdownRequested.IsSet) 'failed running-game query was reported as a deliberate exit'
    Assert-True (-not $unknown.Stream.CanRead) 'unknown shutdown left the pipe open'
    $unknown.Queue.Dispose()

    Write-Host 'PASS Playnite extension mock protocol, identity, bounds, and packaging checks'
} finally {
    Remove-Module $extension -Force
}
