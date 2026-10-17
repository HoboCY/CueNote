param([Parameter(Mandatory=$true)][string]$Run)
$ErrorActionPreference='Stop'
& (Join-Path $PSScriptRoot 'assert-owner.ps1') -Run $Run | Out-Null
$record=Get-Content -LiteralPath (Join-Path $Run 'launcher.json') -Raw | ConvertFrom-Json
$process=Get-Process -Id $record.pid -ErrorAction Stop
$accepted=$process.CloseMainWindow()
$exited=$process.WaitForExit(10000)
$result=[ordered]@{pid=$record.pid;closeAccepted=$accepted;exited=$exited;forced=$false;utc=[DateTime]::UtcNow.ToString('o')}
[IO.File]::AppendAllText((Join-Path $Run 'close-attempts.jsonl'),(($result | ConvertTo-Json -Compress)+"`n"),[Text.UTF8Encoding]::new($false))
if(-not $exited){throw 'Application remains alive; handle its visible save/cancel flow before retrying'}
$result | ConvertTo-Json -Compress
