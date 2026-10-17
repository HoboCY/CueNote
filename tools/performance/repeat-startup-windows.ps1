param(
  [Parameter(Mandatory=$true)][string]$Root,
  [Parameter(Mandatory=$true)][string]$Exe,
  [Parameter(Mandatory=$true)][string]$Sha256,
  [Parameter(Mandatory=$true)][string]$NoteId,
  [int]$Count=10,
  [int]$Port=9371
)
$ErrorActionPreference='Stop'
if($Count -lt 1 -or $Count -gt 30){throw 'Count must be 1..30'}
$driver=Join-Path $PSScriptRoot 'desktop-windows.mjs'
$closer=Join-Path $PSScriptRoot 'close-windows.ps1'
$runs=[Collections.Generic.List[string]]::new()
for($attempt=1;$attempt -le $Count;$attempt++){
  $output=& node.exe $driver launch $Root $Exe $Sha256 $Port 'editor-first-image' $NoteId
  if($LASTEXITCODE -ne 0){throw ('Startup failed at attempt '+$attempt)}
  $result=$output | ConvertFrom-Json
  $runs.Add($result.run)
  [ordered]@{attempt=$attempt;run=$result.run;elapsedMs=$result.elapsedMs} | ConvertTo-Json -Compress
  & $closer -Run $result.run | Out-Null
  if($LASTEXITCODE -ne 0){throw ('Normal close failed at attempt '+$attempt)}
}
