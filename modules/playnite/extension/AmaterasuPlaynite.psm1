# A connection owns its own bounded outbox. Snapshot frames are built with the
# hello nonce before enqueue, so records from an earlier connection cannot be
# restamped after reconnect.
$script:ControlName = 'Sunshine.PlayniteExtension'
$script:ModulePath = $PSCommandPath
$script:Connections = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
$script:ConnectionSlots = [System.Threading.SemaphoreSlim]::new(8, 8)
$script:Signals = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
$script:SnapshotCache = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
$script:Stop = [System.Threading.CancellationTokenSource]::new()
$script:Background = $null
$script:Subscriptions = @()

function Get-SnapshotGeneration($Signals) {
    if (-not $Signals) { return 0L }
    [System.Threading.Monitor]::Enter($Signals)
    try { return [long]$Signals['generation'] } finally { [System.Threading.Monitor]::Exit($Signals) }
}

function Mark-LibraryDirty($Signals) {
    [System.Threading.Monitor]::Enter($Signals)
    try {
        $Signals['generation'] = [long]$Signals['generation'] + 1L
        $Signals['dirty'] = [DateTime]::UtcNow
    } finally { [System.Threading.Monitor]::Exit($Signals) }
}

function Register-LibraryChangeEvent($Source, [string]$EventName, $Signals) {
    # PowerShell runs event actions in a dynamic module. Keep the mutation
    # self-contained because private functions in this module are not visible.
    $sourceId = 'Amaterasu.Playnite.Library.' + [Guid]::NewGuid().ToString('N')
    [void](Register-ObjectEvent -InputObject $Source -EventName $EventName -SourceIdentifier $sourceId -MessageData $Signals -Action {
        $signals = $Event.MessageData
        [System.Threading.Monitor]::Enter($signals)
        try {
            $signals['generation'] = [long]$signals['generation'] + 1L
            $signals['dirty'] = [DateTime]::UtcNow
        } finally { [System.Threading.Monitor]::Exit($signals) }
    })
    return Get-EventSubscriber -SourceIdentifier $sourceId
}

if (-not ('AmaterasuPlaynitePeer' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class AmaterasuPlaynitePeer {
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool GetNamedPipeClientProcessId(SafePipeHandle pipe, out uint pid);
    public static uint ClientPid(SafePipeHandle pipe) {
        uint pid;
        if (!GetNamedPipeClientProcessId(pipe, out pid)) throw new InvalidOperationException("pipe client PID unavailable");
        return pid;
    }
}
'@
}

function New-PrivatePipe([string]$Name) {
    $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    $security = [System.IO.Pipes.PipeSecurity]::new()
    $rule = [System.IO.Pipes.PipeAccessRule]::new($identity.User, [System.IO.Pipes.PipeAccessRights]::FullControl, [System.Security.AccessControl.AccessControlType]::Allow)
    $security.AddAccessRule($rule)
    if ('System.IO.Pipes.NamedPipeServerStreamAcl' -as [type]) {
        return [System.IO.Pipes.NamedPipeServerStreamAcl]::Create($Name, [System.IO.Pipes.PipeDirection]::InOut, 1, [System.IO.Pipes.PipeTransmissionMode]::Byte, [System.IO.Pipes.PipeOptions]::Asynchronous, 65536, 65536, $security, [System.IO.HandleInheritability]::None, [System.IO.Pipes.PipeAccessRights]::ChangePermissions)
    }
    return [System.IO.Pipes.NamedPipeServerStream]::new($Name, [System.IO.Pipes.PipeDirection]::InOut, 1, [System.IO.Pipes.PipeTransmissionMode]::Byte, [System.IO.Pipes.PipeOptions]::Asynchronous, 65536, 65536, $security)
}

function Assert-Hello($Data, $Hello, [uint32]$ControlPid) {
    if (-not $Hello -or $Hello.type -ne 'hello') { throw 'missing typed hello' }
    $pidValue = [uint32]$Hello.pid
    if ($pidValue -eq 0 -or $pidValue -ne $ControlPid -or $pidValue -ne [AmaterasuPlaynitePeer]::ClientPid($Data.SafePipeHandle)) { throw 'control/data pipe client differs from hello PID' }
    Assert-RoleHello $Hello
    $service = Get-CimInstance Win32_Service -Filter "Name='Amaterasu'" -ErrorAction Stop
    $worker = Get-CimInstance Win32_Process -Filter "ProcessId=$pidValue" -ErrorAction Stop
    $serviceProcess = Get-CimInstance Win32_Process -Filter "ProcessId=$($service.ProcessId)" -ErrorAction Stop
    if (-not $service -or -not $worker -or -not $serviceProcess) { throw 'Amaterasu service identity unavailable' }
    Assert-SunshineIdentity $pidValue ([uint32]$service.ProcessId) ([uint32]$worker.ParentProcessId) ([string]$worker.ExecutablePath) ([string]$serviceProcess.ExecutablePath) ([string]$worker.CommandLine)
}

function Assert-RoleHello($Hello) {
    if ($Hello.role -notin @('sunshine', 'launcher')) { throw 'unknown client role' }
    if ($Hello.role -eq 'sunshine') {
        if ([string]$Hello.nonce -notmatch '^[0-9a-f]{32}$') { throw 'invalid Sunshine connection nonce' }
        if ($Hello.gameId -or $Hello.mode) { throw 'Sunshine hello carried launcher fields' }
    } else {
        if ($Hello.nonce) { throw 'launcher hello carried Sunshine nonce' }
        if ($Hello.mode -eq 'fullscreen') {
            if ($Hello.gameId) { throw 'fullscreen watch cannot claim a game ID' }
        } else {
            $parsed = [Guid]::Empty
            if ($Hello.mode -or -not [Guid]::TryParse([string]$Hello.gameId, [ref]$parsed) -or $parsed -eq [Guid]::Empty) { throw 'launcher hello requires a game GUID' }
        }
    }
}

function Assert-SunshineIdentity([uint32]$ClientPid, [uint32]$ServicePid, [uint32]$ParentPid, [string]$ClientPath, [string]$ServicePath, [string]$CommandLine) {
    if ($ClientPid -eq 0 -or $ServicePid -eq 0 -or $ParentPid -ne $ServicePid) { throw 'Sunshine client is not a service child' }
    if (-not [System.IO.Path]::IsPathRooted($ClientPath) -or -not [string]::Equals($ClientPath, $ServicePath, [StringComparison]::OrdinalIgnoreCase)) { throw 'Sunshine client image differs from service image' }
    if ($CommandLine -notmatch '(?i)(?:^|\s)worker-user-serve(?:\s|$)') { throw 'Sunshine client is not an interactive worker' }
}

function Read-BoundedHello($Stream) {
    $bytes = [System.Collections.Generic.List[byte]]::new()
    $one = [byte[]]::new(1)
    $deadline = [DateTime]::UtcNow.AddMilliseconds(1500)
    while ($bytes.Count -lt 8192) {
        $left = [int][Math]::Max(1, ($deadline - [DateTime]::UtcNow).TotalMilliseconds)
        if ([DateTime]::UtcNow -ge $deadline) { throw 'hello timed out' }
        $read = $Stream.ReadAsync($one, 0, 1)
        if (-not $read.Wait($left) -or $read.Result -ne 1) { throw 'hello ended before newline' }
        if ($one[0] -eq 10) { return [System.Text.Encoding]::UTF8.GetString($bytes.ToArray()).TrimEnd("`r") }
        $bytes.Add($one[0])
    }
    throw 'hello exceeded line limit'
}

function Add-Frame($Connection, [object[]]$Lines) {
    # One queue entry contains a whole bracket. A full queue fails closed;
    # no fragment of a snapshot is admitted.
    if ($Lines.Count -gt 512) { return $false }
    $bytes = 0
    foreach ($line in $Lines) {
        $bytes += [System.Text.Encoding]::UTF8.GetByteCount([string]$line) + 1
        if ($bytes -gt 8388608) { return $false }
    }
    return $Connection.Queue.TryAdd($Lines)
}

function Convert-FrameLine($Value) {
    $line = ConvertTo-Json -InputObject $Value -Depth 12 -Compress
    if ([System.Text.Encoding]::UTF8.GetByteCount($line) -gt 1048576) { throw 'frame line exceeds 1 MiB' }
    return $line
}

function Add-CapturedLine($Lines, $Value, [ref]$Bytes) {
    $line = Convert-FrameLine $Value
    $Bytes.Value += [System.Text.Encoding]::UTF8.GetByteCount($line) + 1
    if ($Bytes.Value -gt 8388096) { throw 'snapshot payload exceeds 8 MiB frame budget' }
    $Lines.Add($line)
}

function Get-GameExecutable($Game) {
    $action = @($Game.GameActions | Where-Object { $_.IsPlayAction } | Select-Object -First 1)
    if (-not $action.Count) { $action = @($Game.GameActions | Select-Object -First 1) }
    if (-not $action.Count) { return '' }
    $path = [string]$action[0].Path
    if (-not $path) { return '' }
    if (-not [System.IO.Path]::IsPathRooted($path)) {
        $install = [string]$Game.InstallDirectory
        if (-not [System.IO.Path]::IsPathRooted($install)) { return '' }
        $path = [System.IO.Path]::Combine($install, $path)
    }
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return '' }
    return [System.IO.Path]::GetFullPath($path)
}

function Capture-SnapshotPayload($Api, $Signals = $null) {
    $PlayniteApi = $Api
    if (-not $PlayniteApi.MainView.UIDispatcher.CheckAccess()) { throw 'snapshot capture requires the Playnite UI thread' }
    $generation = Get-SnapshotGeneration $Signals
    $lines = [System.Collections.Generic.List[string]]::new()
    $bytes = 0
    $plugins = @()
    foreach ($plugin in $PlayniteApi.Addons.Plugins) {
        $plugins += @{ id=[string]$plugin.Id; name=[string]$plugin.Name }
        if ($plugins.Count -gt 4096) { throw 'too many plugins' }
    }
    Add-CapturedLine $lines @{ type='plugins'; payload=@($plugins) } ([ref]$bytes)
    $categories = @()
    $categoryNames = @{}
    foreach ($category in $PlayniteApi.Database.Categories) {
        $id = [string]$category.Id
        $categories += @{ id=$id; name=[string]$category.Name }
        $categoryNames[$id] = [string]$category.Name
        if ($categories.Count -gt 4096) { throw 'too many categories' }
    }
    Add-CapturedLine $lines @{ type='categories'; payload=@($categories) } ([ref]$bytes)
    $count = 0
    $batch = [System.Collections.Generic.List[object]]::new()
    foreach ($game in $PlayniteApi.Database.Games) {
        if (++$count -gt 50000) { throw 'too many games' }
        $names = @()
        foreach ($id in @($game.CategoryIds)) { if ($categoryNames.ContainsKey([string]$id)) { $names += $categoryNames[[string]$id] } }
        $lastPlayed = if ($game.LastActivity) { ([DateTime]$game.LastActivity).ToString('o') } else { '' }
        $batch.Add(@{ id=[string]$game.Id; name=[string]$game.Name; exe=(Get-GameExecutable $game); args=''; workingDir=''; installDir=[string]$game.InstallDirectory; categories=@($names); pluginId=[string]$game.PluginId; pluginName=''; lastPlayed=$lastPlayed; installed=[bool]$game.IsInstalled })
        if ($batch.Count -eq 100) {
            Add-CapturedLine $lines @{ type='games'; payload=$batch.ToArray() } ([ref]$bytes)
            $batch.Clear()
        }
    }
    if ($batch.Count) { Add-CapturedLine $lines @{ type='games'; payload=$batch.ToArray() } ([ref]$bytes) }
    if ($lines.Count -gt 510) { throw 'snapshot exceeds frame count' }
    if ($generation -ne (Get-SnapshotGeneration $Signals)) { throw 'Playnite library changed during snapshot capture' }
    return @{ Lines=$lines.ToArray(); Games=$count; CapturedAt=[DateTime]::UtcNow; Generation=$generation }
}

function New-SnapshotFrame($Connection, [string]$RequestId = '', $Payload = $null) {
    if (-not $Payload -and -not $Connection.Cache.TryGetValue('payload', [ref]$Payload)) { throw 'no complete UI-thread snapshot exists' }
    $dirtyAt = [DateTime]::MinValue
    [void]$Connection.Signals.TryGetValue('dirty', [ref]$dirtyAt)
    if ($Payload.CapturedAt -lt $dirtyAt -or $Payload.Generation -ne (Get-SnapshotGeneration $Connection.Signals)) { throw 'Playnite library changed after cached snapshot' }
    $marker = @{ type='snapshotStart'; nonce=[string]$Connection.Nonce }
    if ($RequestId) { $marker.requestId = $RequestId }
    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add((Convert-FrameLine $marker))
    foreach ($line in [string[]]$Payload.Lines) { $lines.Add($line) }
    $end = @{ type='snapshotComplete'; nonce=[string]$Connection.Nonce; games=[int]$Payload.Games }
    if ($RequestId) { $end.requestId = $RequestId }
    $lines.Add((Convert-FrameLine $end))
    return $lines.ToArray()
}

function Add-SnapshotFrame($Connection, [string[]]$Lines, [DateTime]$CapturedAt, [long]$Generation, [string]$RequestId = '') {
    # Keep the capture generation with the bracket until the writer finishes.
    # A queued frame can become stale while a library event is being handled.
    if ($Lines.Count -gt 512) { return $false }
    $bytes = 0
    foreach ($line in $Lines) {
        $bytes += [System.Text.Encoding]::UTF8.GetByteCount($line) + 1
        if ($bytes -gt 8388608) { return $false }
    }
    return $Connection.Queue.TryAdd(@{ Lines=$Lines; CapturedAt=$CapturedAt; Generation=$Generation; RequestId=$RequestId })
}

function Write-QueuedFrame($Connection, $Frame, $Writer) {
    $snapshot = $Frame -is [System.Collections.IDictionary]
    $lines = if ($snapshot) { [string[]]$Frame.Lines } else { [string[]]$Frame }
    for ($index = 0; $index -lt $lines.Count; $index++) {
        $line = $lines[$index]
        if ($snapshot) {
            if ($index -ne $lines.Count - 1) {
                $dirtyAt = [DateTime]::MinValue
                [void]$Connection.Signals.TryGetValue('dirty', [ref]$dirtyAt)
                if ($dirtyAt -gt $Frame.CapturedAt -or [long]$Connection.Signals['generation'] -ne $Frame.Generation) { return $false }
                $Writer.WriteLine($line)
                continue
            }
            if ($Frame.BeforeComplete) { & $Frame.BeforeComplete }
            [System.Threading.Monitor]::Enter($Connection.Signals)
            try {
                $dirtyAt = [DateTime]::MinValue
                [void]$Connection.Signals.TryGetValue('dirty', [ref]$dirtyAt)
                if ($dirtyAt -gt $Frame.CapturedAt -or [long]$Connection.Signals['generation'] -ne $Frame.Generation) { return $false }
                if ($Writer -is [System.IO.StreamWriter]) {
                    # The completion and dirty mark share one ordering point. Bound
                    # the only pipe write performed while holding the UI signal lock.
                    if (-not $Writer.WriteLineAsync([string]$line).Wait(1000)) {
                        $Connection.Stream.Dispose()
                        throw 'snapshot completion write timed out'
                    }
                } else { $Writer.WriteLine($line) }
            } finally { [System.Threading.Monitor]::Exit($Connection.Signals) }
        } else {
            $Writer.WriteLine($line)
        }
    }
    return $true
}

function Queue-Snapshot($Connection, [string]$RequestId = '') {
    try {
        $dirtyAt = [DateTime]::MinValue
        [void]$Connection.Signals.TryGetValue('dirty', [ref]$dirtyAt)
        $cached = $null
        $hasCache = $Connection.Cache.TryGetValue('payload', [ref]$cached)
        if (-not $hasCache -or $cached.CapturedAt -lt $dirtyAt -or $cached.Generation -ne (Get-SnapshotGeneration $Connection.Signals)) {
            $api = $Connection.Api
            $signals = $Connection.Signals
            $fresh = $Connection.Dispatcher.Invoke([Func[object]]{ Capture-SnapshotPayload $api $signals })
            if (-not $fresh -or $fresh.CapturedAt -lt $dirtyAt) { throw 'library changed during UI capture' }
            $Connection.Cache['payload'] = $fresh
        }
        $payload = $null
        if (-not $Connection.Cache.TryGetValue('payload', [ref]$payload)) { throw 'no complete UI-thread snapshot exists' }
        $frame = [string[]]@(New-SnapshotFrame $Connection $RequestId $payload)
        if (-not (Add-SnapshotFrame $Connection $frame $payload.CapturedAt $payload.Generation $RequestId)) { throw 'snapshot outbox full' }
    } catch { $Connection.Stream.Dispose() }
}

function Send-Status([string]$Name, $Game) {
    $status = @{ type='status'; status=@{ name=$Name; id=[string]$Game.Id; installDir=[string]$Game.InstallDirectory; exe=(Get-GameExecutable $Game) } }
    $line = Convert-FrameLine $status
    foreach ($entry in $script:Connections.Values) {
        if ($entry.Role -eq 'sunshine' -or ($entry.Role -eq 'launcher' -and ($entry.Mode -eq 'fullscreen' -or $entry.GameId -eq [string]$Game.Id))) {
            if (-not (Add-Frame $entry @($line))) { $entry.Stream.Dispose() }
        }
    }
}

function Replay-RunningGame($Connection) {
    if ($Connection.Role -ne 'launcher') { return }
    if ($Connection.Mode -ne 'fullscreen' -and -not $Connection.GameId) { return }
    try {
        $api = $Connection.Api
        $id = if ($Connection.Mode -eq 'fullscreen') { [Guid]::Empty } else { [Guid]::Parse($Connection.GameId) }
        $queued = $Connection.Dispatcher.Invoke([Func[object]]{
            $running = if ($Connection.Mode -eq 'fullscreen') {
                # Two distinct games are enough for the watcher to reject an
                # ambiguous return. Queue both in one frame on the UI thread,
                # ordered with OnGameStarted and OnGameStopped callbacks.
                @($api.Database.Games | Where-Object { $_.IsRunning } | Select-Object -First 2)
            } else {
                $game = $api.Database.Games.Get($id)
                if ($game -and $game.IsRunning) { @($game) } else { @() }
            }
            $lines = [System.Collections.Generic.List[string]]::new()
            foreach ($game in $running) {
                $lines.Add((Convert-FrameLine @{ type='status'; status=@{ name='gameStarted'; id=[string]$game.Id; installDir=[string]$game.InstallDirectory; exe=(Get-GameExecutable $game) } }))
            }
            if ($lines.Count -eq 0) { return $true }
            return (Add-Frame $Connection $lines.ToArray())
        })
        if (-not $queued) { throw 'running-game replay queue full' }
    } catch { $Connection.Stream.Dispose() }
}

function Invoke-ExtensionCommand($Connection, $Message) {
    if ($Message.type -ne 'command' -or $Connection.Role -ne 'sunshine') { return }
    $requestId = [string]$Message.requestId
    if ($Message.command -in @('launch', 'stop')) {
        $deadline = 0L
        if ($requestId -notmatch '^[0-9a-f]{32}$' -or
            -not [long]::TryParse([string]$Message.deadlineUnixMs, [ref]$deadline) -or
            [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -ge $deadline) {
            if ($requestId -match '^[0-9a-f]{32}$') {
                [void](Add-Frame $Connection @((Convert-FrameLine @{ type='commandResult'; command=[string]$Message.command; requestId=$requestId; nonce=$Connection.Nonce; success=$false; error='expired or invalid command deadline' })))
            }
            return
        }
    }
    switch ([string]$Message.command) {
        'snapshot' { if ($requestId -match '^[0-9a-f]{32}$') { Queue-Snapshot $Connection $requestId }; break }
        'launch' {
            $success = $false
            try {
                $id = [Guid]::Parse([string]$Message.id)
                $api = $Connection.Api
                $success = [bool]$Connection.Dispatcher.Invoke([Func[bool]]{
                    if (-not $api.Database.Games.Get($id)) { return $false }
                    $api.StartGame($id)
                    return $true
                })
            } catch {}
            if ($requestId) { [void](Add-Frame $Connection @((Convert-FrameLine @{ type='commandResult'; command='launch'; requestId=$requestId; nonce=$Connection.Nonce; success=$success }))) }
            break
        }
        'stop' {
            # A launcher is responsible for the stream stop; send it a status.
            $target = [string]$Message.id
            $delivered = $false
            foreach ($entry in $script:Connections.Values) {
                if ($entry.Role -eq 'launcher' -and $entry.GameId -eq $target) {
                    if (Add-Frame $entry @((Convert-FrameLine @{ type='status'; status=@{ name='stopRequested'; id=$target } }))) { $delivered = $true }
                }
            }
            # This asks the launcher to stop the stream. It does not kill the
            # game; recovered sessions still require a gameStopped observation.
            if ($requestId) { [void](Add-Frame $Connection @((Convert-FrameLine @{ type='commandResult'; command='stop'; requestId=$requestId; nonce=$Connection.Nonce; success=$delivered }))) }
            break
        }
    }
}

function Write-FullscreenShutdown($Connection, $Writer) {
    if (-not $Connection.ShutdownRequested.IsSet -or $Connection.ShutdownWritten.IsSet) { return $false }
    $Writer.WriteLine((Convert-FrameLine @{ type='status'; status=@{ name='playniteExiting' } }))
    $Writer.Flush()
    $Connection.ShutdownWritten.Set()
    return $true
}

function Request-FullscreenShutdown($Connections) {
    $pending = [System.Collections.Generic.List[object]]::new()
    foreach ($connection in $Connections.Values) {
        if ($connection.Role -eq 'launcher' -and $connection.Mode -eq 'fullscreen' -and $connection.ShutdownRequested -and $connection.ShutdownWritten) {
            $connection.ShutdownRequested.Set()
            $pending.Add($connection)
        }
    }
    return $pending.ToArray()
}

function Await-FullscreenShutdown([object[]]$Connections, [int]$TimeoutMilliseconds) {
    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    foreach ($connection in $Connections) {
        $remaining = [int][Math]::Max(0, ($deadline - [DateTime]::UtcNow).TotalMilliseconds)
        [void]$connection.ShutdownWritten.Wait($remaining)
    }
}

function Serve-DataConnection($Stream, $Api, $Dispatcher, $Connections, $Stop, $Signals, $Cache, $Slots, [uint32]$ControlPid) {
    $script:Connections = $Connections
    $script:Stop = $Stop
    $PlayniteApi = $Api
    $reader = [System.IO.StreamReader]::new($Stream, [System.Text.Encoding]::UTF8, $false, 8192, $true)
    $writer = [System.IO.StreamWriter]::new($Stream, [System.Text.Encoding]::UTF8, 8192, $true)
    $writer.AutoFlush = $true
    $key = [Guid]::NewGuid().ToString('N')
    try {
        $hello = ConvertFrom-Json -InputObject (Read-BoundedHello $Stream) -ErrorAction Stop
        Assert-Hello $Stream $hello $ControlPid
        $connection = @{ Role=[string]$hello.role; Nonce=[string]$hello.nonce; GameId=[string]$hello.gameId; Mode=[string]$hello.mode; Stream=$Stream; Api=$Api; Dispatcher=$Dispatcher; Signals=$Signals; Cache=$Cache; Queue=[System.Collections.Concurrent.BlockingCollection[object]]::new(8); ShutdownRequested=[System.Threading.ManualResetEventSlim]::new($false); ShutdownWritten=[System.Threading.ManualResetEventSlim]::new($false); LastDirty=[DateTime]::MinValue }
        if ($connection.Role -eq 'sunshine') {
            foreach ($old in $Connections.Values) { if ($old.Role -eq 'sunshine') { $old.Stream.Dispose() } }
        }
        if (-not $Connections.TryAdd($key, $connection)) { throw 'connection registration failed' }
        if ($connection.Role -eq 'sunshine') { Queue-Snapshot $connection }
        if ($connection.Role -eq 'launcher') { Replay-RunningGame $connection }
        $read = $reader.ReadLineAsync()
        while (-not $Stop.IsCancellationRequested -and $Stream.IsConnected) {
            if (Write-FullscreenShutdown $connection $writer) { break }
            if ($read.IsCompleted) {
                $line = $read.GetAwaiter().GetResult()
                if ($null -eq $line) { break }
                if ($line.Length -gt 8192) { break }
                try { Invoke-ExtensionCommand $connection (ConvertFrom-Json -InputObject $line -ErrorAction Stop) } catch {}
                $read = $reader.ReadLineAsync()
            }
            $dirtyAt = [DateTime]::MinValue
            [void]$Signals.TryGetValue('dirty', [ref]$dirtyAt)
            if ($connection.Role -eq 'sunshine' -and $dirtyAt -gt $connection.LastDirty -and [DateTime]::UtcNow -ge $dirtyAt.AddSeconds(3)) {
                $connection.LastDirty = $dirtyAt
                Queue-Snapshot $connection
            }
            if (Write-FullscreenShutdown $connection $writer) { break }
            $frame = $null
            if ($connection.Queue.TryTake([ref]$frame, 100)) {
                if (-not (Write-QueuedFrame $connection $frame $writer) -and $frame.RequestId) {
                    Queue-Snapshot $connection $frame.RequestId
                }
            }
        }
    } finally {
        $removed = $null
        [void]$Connections.TryRemove($key, [ref]$removed)
        $Stream.Dispose()
    }
}

function Start-PipeServer($Api, $Dispatcher, $Connections, $Stop, $Signals, $Cache, $Slots) {
    $active = [System.Collections.Generic.List[object]]::new()
    while (-not $Stop.IsCancellationRequested) {
        for ($index = $active.Count - 1; $index -ge 0; $index--) {
            $item = $active[$index]
            if ($item.Handle.IsCompleted) {
                try { [void]$item.Worker.EndInvoke($item.Handle) } catch {}
                $item.Worker.Dispose(); $item.Runspace.Dispose()
                $active.RemoveAt($index)
            }
        }
        $control = $null
        $data = $null
        $slotOwned = $false
        try {
            $control = New-PrivatePipe $script:ControlName
            $wait = $control.WaitForConnectionAsync()
            while (-not $wait.Wait(100)) { if ($Stop.IsCancellationRequested) { throw 'stopping' } }
            $controlPid = [AmaterasuPlaynitePeer]::ClientPid($control.SafePipeHandle)
            $name = [Guid]::NewGuid().ToString('B').ToUpperInvariant()
            $data = New-PrivatePipe $name
            $handshake = [byte[]]::new(80)
            [void][System.Text.Encoding]::Unicode.GetBytes($name, 0, $name.Length, $handshake, 0)
            $control.Write($handshake, 0, 80)
            $control.Flush()
            $ack = [byte[]]::new(1)
            $ackRead = $control.ReadAsync($ack, 0, 1)
            if (-not $ackRead.Wait(1500) -or $ackRead.Result -ne 1 -or $ack[0] -ne 2) { throw 'handshake ACK missing' }
            $control.Dispose(); $control = $null
            $wait = $data.WaitForConnectionAsync()
            $deadline = [DateTime]::UtcNow.AddSeconds(8)
            while (-not $wait.Wait(100)) {
                if ($Stop.IsCancellationRequested) { throw 'stopping' }
                if ([DateTime]::UtcNow -ge $deadline) { throw 'data pipe connect timed out' }
            }
            if (-not $Slots.Wait(0)) { throw 'connection limit reached' }
            $slotOwned = $true
            $runspace = [System.Management.Automation.Runspaces.RunspaceFactory]::CreateRunspace()
            $runspace.Open()
            $worker = [PowerShell]::Create()
            $worker.Runspace = $runspace
            [void]$worker.AddScript('param($modulePath,$stream,$api,$dispatcher,$connections,$stop,$signals,$cache,$slots,$controlPid) try { Import-Module -Name $modulePath -Force -ErrorAction Stop; Serve-DataConnection $stream $api $dispatcher $connections $stop $signals $cache $slots $controlPid } finally { [void]$slots.Release(); $stream.Dispose() }').AddArgument($script:ModulePath).AddArgument($data).AddArgument($Api).AddArgument($Dispatcher).AddArgument($Connections).AddArgument($Stop).AddArgument($Signals).AddArgument($Cache).AddArgument($Slots).AddArgument($controlPid)
            $active.Add(@{ Worker=$worker; Runspace=$runspace; Handle=$worker.BeginInvoke() })
            $slotOwned = $false
            $data = $null
        } catch {
            if (-not $Stop.IsCancellationRequested) { Start-Sleep -Milliseconds 200 }
        } finally {
            if ($control) { $control.Dispose() }
            if ($data) { $data.Dispose() }
            if ($slotOwned) { [void]$Slots.Release() }
        }
    }
    foreach ($item in $active) {
        try { [void]$item.Handle.AsyncWaitHandle.WaitOne(1500) } catch {}
        try { $item.Worker.Dispose(); $item.Runspace.Dispose() } catch {}
    }
}

function OnApplicationStarted() {
    if ($script:Background) { return }
    try { $script:SnapshotCache['payload'] = Capture-SnapshotPayload $PlayniteApi $script:Signals } catch { $discard = $null; [void]$script:SnapshotCache.TryRemove('payload', [ref]$discard) }
    $script:Stop = [System.Threading.CancellationTokenSource]::new()
    $runspace = [System.Management.Automation.Runspaces.RunspaceFactory]::CreateRunspace()
    $runspace.Open()
    $worker = [PowerShell]::Create(); $worker.Runspace = $runspace
    $dispatcher = $PlayniteApi.MainView.UIDispatcher
    [void]$worker.AddScript('param($modulePath,$api,$dispatcher,$connections,$stop,$signals,$cache,$slots) Import-Module -Name $modulePath -Force; Start-PipeServer $api $dispatcher $connections $stop $signals $cache $slots').AddArgument($script:ModulePath).AddArgument($PlayniteApi).AddArgument($dispatcher).AddArgument($script:Connections).AddArgument($script:Stop).AddArgument($script:Signals).AddArgument($script:SnapshotCache).AddArgument($script:ConnectionSlots)
    $script:Background = @{ Runspace=$runspace; Worker=$worker; Handle=$worker.BeginInvoke() }
    foreach ($eventName in @('ItemCollectionChanged','ItemUpdated')) {
        try { $script:Subscriptions += Register-LibraryChangeEvent $PlayniteApi.Database.Games $eventName $script:Signals } catch {}
    }
}

function OnGameStarted($evnArgs) { Send-Status 'gameStarted' $evnArgs.Game }
function OnGameStopped($evnArgs) { Send-Status 'gameStopped' $evnArgs.Game }
function OnLibraryUpdated() {
    Mark-LibraryDirty $script:Signals
    try {
        $payload = Capture-SnapshotPayload $PlayniteApi $script:Signals
        $script:SnapshotCache['payload'] = $payload
    } catch {
        $discard = $null
        [void]$script:SnapshotCache.TryRemove('payload', [ref]$discard)
    }
}

function OnApplicationStopped() {
    $pending = @(Request-FullscreenShutdown $script:Connections)
    Await-FullscreenShutdown $pending 500
    $script:Stop.Cancel()
    foreach ($connection in $script:Connections.Values) { $connection.Stream.Dispose() }
    foreach ($subscription in $script:Subscriptions) { Unregister-Event -SubscriptionId $subscription.SubscriptionId -ErrorAction SilentlyContinue }
    if ($script:Background) {
        $script:Background.Worker.Dispose()
        $script:Background.Runspace.Dispose()
        $script:Background = $null
    }
}

Export-ModuleMember -Function OnApplicationStarted,OnApplicationStopped,OnGameStarted,OnGameStopped,OnLibraryUpdated,Start-PipeServer,Serve-DataConnection
