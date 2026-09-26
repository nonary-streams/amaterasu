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
        Signals = [System.Collections.Concurrent.ConcurrentDictionary[string,DateTime]]::new()
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
        $ambiguous = [string[]]($new.Queue.Take())
        Assert-Equal $ambiguous.Count 2 'ambiguous running games were not both reported'
        Assert-True (($ambiguous[0] | ConvertFrom-Json).status.id -cne ($ambiguous[1] | ConvertFrom-Json).status.id) 'ambiguous games collapsed into one status'
        $api.Database.Games = @($api.Database.Games[0])
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

    Write-Host 'PASS Playnite extension mock protocol, identity, bounds, and packaging checks'
} finally {
    Remove-Module $extension -Force
}
