param([Parameter(Mandatory=$true)][string]$Run)
$ErrorActionPreference='Stop'
$record=Get-Content -LiteralPath (Join-Path $Run 'launcher.json') -Raw | ConvertFrom-Json
if([IO.Path]::GetFullPath($record.run) -ne [IO.Path]::GetFullPath($Run)){throw 'Run identity mismatch'}
$process=Get-Process -Id $record.pid -ErrorAction Stop
if([IO.Path]::GetFullPath($process.Path) -ne [IO.Path]::GetFullPath($record.exe)){throw 'Executable identity mismatch'}
if((Get-FileHash -LiteralPath $process.Path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $record.sha256){throw 'Executable hash mismatch'}
$all=@(Get-CimInstance Win32_Process)
$descendants=[Collections.Generic.HashSet[int]]::new()
[void]$descendants.Add([int]$record.pid)
do {
  $added=$false
  foreach($item in $all){if($descendants.Contains([int]$item.ParentProcessId) -and $descendants.Add([int]$item.ProcessId)){$added=$true}}
}while($added)
$listeners=@(Get-NetTCPConnection -LocalPort $record.port -State Listen -ErrorAction Stop)
if($listeners.Count -eq 0){throw 'Debugger listener missing'}
foreach($listener in $listeners){
  if(-not $descendants.Contains([int]$listener.OwningProcess)){throw 'Debugger belongs to another process'}
  $owner=$all | Where-Object ProcessId -eq $listener.OwningProcess
  if(-not $owner.CommandLine.Contains([string]$record.profile)){throw 'Debugger profile mismatch'}
}
Write-Output 'owned'
