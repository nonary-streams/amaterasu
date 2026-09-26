# A connection owns its own bounded outbox. Snapshot frames are built with the
# hello nonce before enqueue, so records from an earlier connection cannot be
# restamped after reconnect.
$script:ControlName = 'Sunshine.PlayniteExtension'
$script:ModulePath = $PSCommandPath
$script:Connections = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
$script:ConnectionSlots = [System.Threading.SemaphoreSlim]::new(8, 8)
$script:Signals = [System.Collections.Concurrent.ConcurrentDictionary[string,DateTime]]::new()
$script:SnapshotCache = [System.Collections.Concurrent.ConcurrentDictionary[string,object]]::new()
$script:Stop = [System.Threading.CancellationTokenSource]::new()
$script:Background = $null
$script:Subscriptions = @()

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
    if ($Hello.role -notin @('sunshine', 'launcher')) { throw 'unknown client role' }
    if ($Hello.role -eq 'sunshine') {
        if ([string]$Hello.nonce -notmatch '^[0-9a-f]{32}$') { throw 'invalid Sunshine connection nonce' }
        $service = Get-CimInstance Win32_Service -Filter "Name='Amaterasu'" -ErrorAction Stop
        $worker = Get-CimInstance Win32_Process -Filter "ProcessId=$pidValue" -ErrorAction Stop
        $serviceProcess = Get-CimInstance Win32_Process -Filter "ProcessId=$($service.ProcessId)" -ErrorAction Stop
        if (-not $service -or -not $worker -or -not $serviceProcess) { throw 'Amaterasu service identity unavailable' }
        Assert-SunshineIdentity $pidValue ([uint32]$service.ProcessId) ([uint32]$worker.ParentProcessId) ([string]$worker.ExecutablePath) ([string]$serviceProcess.ExecutablePath) ([string]$worker.CommandLine)
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

function Capture-SnapshotPayload($Api) {
    $PlayniteApi = $Api
    if (-not $PlayniteApi.MainView.UIDispatcher.CheckAccess()) { throw 'snapshot capture requires the Playnite UI thread' }
    $lines = [System.Collections.Generic.List[string]]::new()
    $plugins = @()
    foreach ($plugin in @($PlayniteApi.Addons.Plugins)) {
        $plugins += @{ id=[string]$plugin.Id; name=[string]$plugin.Name }
        if ($plugins.Count -gt 4096) { throw 'too many plugins' }
    }
    $lines.Add((Convert-FrameLine @{ type='plugins'; payload=@($plugins) }))
    $categories = @()
    $categoryNames = @{}
    foreach ($category in @($PlayniteApi.Database.Categories)) {
        $id = [string]$category.Id
        $categories += @{ id=$id; name=[string]$category.Name }
        $categoryNames[$id] = [string]$category.Name
        if ($categories.Count -gt 4096) { throw 'too many categories' }
    }
    $lines.Add((Convert-FrameLine @{ type='categories'; payload=@($categories) }))
    $count = 0
    $batch = [System.Collections.Generic.List[object]]::new()
    foreach ($game in @($PlayniteApi.Database.Games)) {
        if (++$count -gt 50000) { throw 'too many games' }
        $names = @()
        foreach ($id in @($game.CategoryIds)) { if ($categoryNames.ContainsKey([string]$id)) { $names += $categoryNames[[string]$id] } }
        $lastPlayed = if ($game.LastActivity) { ([DateTime]$game.LastActivity).ToString('o') } else { '' }
        $batch.Add(@{ id=[string]$game.Id; name=[string]$game.Name; exe=(Get-GameExecutable $game); args=''; workingDir=''; installDir=[string]$game.InstallDirectory; categories=@($names); pluginId=[string]$game.PluginId; pluginName=''; lastPlayed=$lastPlayed; installed=[bool]$game.IsInstalled })
        if ($batch.Count -eq 100) {
            $lines.Add((Convert-FrameLine @{ type='games'; payload=$batch.ToArray() }))
            $batch.Clear()
        }
    }
    if ($batch.Count) { $lines.Add((Convert-FrameLine @{ type='games'; payload=$batch.ToArray() })) }
    if ($lines.Count -gt 510) { throw 'snapshot exceeds frame count' }
    return @{ Lines=$lines.ToArray(); Games=$count; CapturedAt=[DateTime]::UtcNow }
}

function New-SnapshotFrame($Connection, [string]$RequestId = '') {
    $payload = $null
    if (-not $Connection.Cache.TryGetValue('payload', [ref]$payload)) { throw 'no complete UI-thread snapshot exists' }
    $dirtyAt = [DateTime]::MinValue
    [void]$Connection.Signals.TryGetValue('dirty', [ref]$dirtyAt)
    if ($payload.CapturedAt -lt $dirtyAt) { throw 'Playnite library changed after cached snapshot' }
    $marker = @{ type='snapshotStart'; nonce=[string]$Connection.Nonce }
    if ($RequestId) { $marker.requestId = $RequestId }
    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add((Convert-FrameLine $marker))
    foreach ($line in [string[]]$payload.Lines) { $lines.Add($line) }
    $end = @{ type='snapshotComplete'; nonce=[string]$Connection.Nonce; games=[int]$payload.Games }
    if ($RequestId) { $end.requestId = $RequestId }
    $lines.Add((Convert-FrameLine $end))
    return $lines.ToArray()
}

function Queue-Snapshot($Connection, [string]$RequestId = '') {
    try {
        $dirtyAt = [DateTime]::MinValue
        [void]$Connection.Signals.TryGetValue('dirty', [ref]$dirtyAt)
        $cached = $null
        $hasCache = $Connection.Cache.TryGetValue('payload', [ref]$cached)
        if (-not $hasCache -or $cached.CapturedAt -lt $dirtyAt) {
            $api = $Connection.Api
            $fresh = $Connection.Dispatcher.Invoke([Func[object]]{ Capture-SnapshotPayload $api })
            if (-not $fresh -or $fresh.CapturedAt -lt $dirtyAt) { throw 'library changed during UI capture' }
            $Connection.Cache['payload'] = $fresh
        }
        $frame = @(New-SnapshotFrame $Connection $RequestId)
        if (-not (Add-Frame $Connection $frame)) { throw 'snapshot outbox full' }
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
    if ($Connection.Role -ne 'launcher' -or -not $Connection.GameId) { return }
    try {
        $id = [Guid]::Parse($Connection.GameId)
        $api = $Connection.Api
        $status = $Connection.Dispatcher.Invoke([Func[object]]{
            $game = $api.Database.Games.Get($id)
            if (-not $game -or -not $game.IsRunning) { return $null }
            return @{ type='status'; status=@{ name='gameStarted'; id=[string]$game.Id; installDir=[string]$game.InstallDirectory; exe=(Get-GameExecutable $game) } }
        })
        if ($status -and -not (Add-Frame $Connection @((Convert-FrameLine $status)))) { throw 'running-game replay queue full' }
    } catch { $Connection.Stream.Dispose() }
}

function Invoke-ExtensionCommand($Connection, $Message) {
    if ($Message.type -ne 'command' -or $Connection.Role -ne 'sunshine') { return }
    $requestId = [string]$Message.requestId
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
        $connection = @{ Role=[string]$hello.role; Nonce=[string]$hello.nonce; GameId=[string]$hello.gameId; Mode=[string]$hello.mode; Stream=$Stream; Api=$Api; Dispatcher=$Dispatcher; Signals=$Signals; Cache=$Cache; Queue=[System.Collections.Concurrent.BlockingCollection[object]]::new(8); LastDirty=[DateTime]::MinValue }
        if ($connection.Role -eq 'sunshine') {
            foreach ($old in $Connections.Values) { if ($old.Role -eq 'sunshine') { $old.Stream.Dispose() } }
        }
        if (-not $Connections.TryAdd($key, $connection)) { throw 'connection registration failed' }
        if ($connection.Role -eq 'sunshine') { Queue-Snapshot $connection }
        if ($connection.Role -eq 'launcher') { Replay-RunningGame $connection }
        $read = $reader.ReadLineAsync()
        while (-not $Stop.IsCancellationRequested -and $Stream.IsConnected) {
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
            $frame = $null
            if ($connection.Queue.TryTake([ref]$frame, 100)) {
                foreach ($line in [string[]]$frame) { $writer.WriteLine($line) }
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
    try { $script:SnapshotCache['payload'] = Capture-SnapshotPayload $PlayniteApi } catch { $discard = $null; [void]$script:SnapshotCache.TryRemove('payload', [ref]$discard) }
    $script:Stop = [System.Threading.CancellationTokenSource]::new()
    $runspace = [System.Management.Automation.Runspaces.RunspaceFactory]::CreateRunspace()
    $runspace.Open()
    $worker = [PowerShell]::Create(); $worker.Runspace = $runspace
    $dispatcher = $PlayniteApi.MainView.UIDispatcher
    [void]$worker.AddScript('param($modulePath,$api,$dispatcher,$connections,$stop,$signals,$cache,$slots) Import-Module -Name $modulePath -Force; Start-PipeServer $api $dispatcher $connections $stop $signals $cache $slots').AddArgument($script:ModulePath).AddArgument($PlayniteApi).AddArgument($dispatcher).AddArgument($script:Connections).AddArgument($script:Stop).AddArgument($script:Signals).AddArgument($script:SnapshotCache).AddArgument($script:ConnectionSlots)
    $script:Background = @{ Runspace=$runspace; Worker=$worker; Handle=$worker.BeginInvoke() }
    foreach ($eventName in @('ItemCollectionChanged','ItemUpdated')) {
        try { $script:Subscriptions += Register-ObjectEvent -InputObject $PlayniteApi.Database.Games -EventName $eventName -MessageData $script:Signals -Action { $Event.MessageData['dirty'] = [DateTime]::UtcNow } } catch {}
    }
}

function OnGameStarted($evnArgs) { Send-Status 'gameStarted' $evnArgs.Game }
function OnGameStopped($evnArgs) { Send-Status 'gameStopped' $evnArgs.Game }
function OnLibraryUpdated() {
    try {
        $payload = Capture-SnapshotPayload $PlayniteApi
        $script:SnapshotCache['payload'] = $payload
        $script:Signals['dirty'] = $payload.CapturedAt
    } catch {
        $discard = $null
        [void]$script:SnapshotCache.TryRemove('payload', [ref]$discard)
    }
}

function OnApplicationStopped() {
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
