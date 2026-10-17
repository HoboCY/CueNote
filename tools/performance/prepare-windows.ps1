param(
  [Parameter(Mandatory=$true)][string]$Corpus,
  [Parameter(Mandatory=$true)][string]$Destination
)
$ErrorActionPreference='Stop'
if(-not [IO.Path]::IsPathFullyQualified($Corpus) -or -not [IO.Path]::IsPathFullyQualified($Destination)){throw 'Absolute paths required'}
$source=[IO.Path]::GetFullPath($Corpus)
$target=[IO.Path]::GetFullPath($Destination)
if(Test-Path -LiteralPath $target){throw 'Refusing to overwrite an existing destination'}
if(-not (Test-Path -LiteralPath (Join-Path $source 'manifest.json') -PathType Leaf)){throw 'Corpus manifest missing'}
if(-not (Test-Path -LiteralPath (Join-Path $source 'library/notes') -PathType Container)){throw 'Corpus library missing'}
if($target.StartsWith($source.TrimEnd('\')+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Keep the disposable copy outside the immutable corpus'}
New-Item -ItemType Directory -Path $target | Out-Null
Copy-Item -LiteralPath (Join-Path $source 'library') -Destination (Join-Path $target 'library') -Recurse -ErrorAction Stop
# Copy, never hard-link: generated fixtures remain immutable even if a test fails.
$manifest=Get-Content -LiteralPath (Join-Path $source 'manifest.json') -Raw | ConvertFrom-Json
$expectedFiles=@($manifest.files | Where-Object { $_.path -like 'library/*' })
foreach($entry in $expectedFiles){
  $file=[IO.Path]::GetFullPath((Join-Path $target ([string]$entry.path)))
  if(-not $file.StartsWith((Join-Path $target 'library')+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Manifest path escaped the copied library'}
  if((Get-Item -LiteralPath $file).Length -ne $entry.bytes -or (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant() -ne $entry.sha256){throw ('Copied file does not match corpus manifest: '+$entry.path)}
}
$actualFiles=@(Get-ChildItem -LiteralPath (Join-Path $target 'library') -File -Recurse)
if($actualFiles.Count -ne $expectedFiles.Count){throw 'Copied library contains an unexpected file set'}
[IO.File]::WriteAllText((Join-Path $target '.cuenote-performance'),"CueNote isolated performance root v1`n",[Text.UTF8Encoding]::new($false))
$receipt=[ordered]@{source=$source;destination=$target;manifestSha256=(Get-FileHash -LiteralPath (Join-Path $source 'manifest.json') -Algorithm SHA256).Hash.ToLowerInvariant();createdUtc=[DateTime]::UtcNow.ToString('o')}
[IO.File]::WriteAllText((Join-Path $target 'corpus-copy.json'),($receipt | ConvertTo-Json),[Text.UTF8Encoding]::new($false))
$receipt | ConvertTo-Json
