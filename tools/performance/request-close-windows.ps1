param([Parameter(Mandatory=$true)][string]$Run)
$ErrorActionPreference='Stop'
& (Join-Path $PSScriptRoot 'assert-owner.ps1') -Run $Run | Out-Null
$record=Get-Content -LiteralPath (Join-Path $Run 'launcher.json') -Raw | ConvertFrom-Json
$process=Get-Process -Id ([int]$record.pid) -ErrorAction Stop
if(-not $process.CloseMainWindow()){throw 'Owned application did not accept a normal close request'}
# Do not wait for exit: the UI driver verifies and cancels the save-wait dialog.
