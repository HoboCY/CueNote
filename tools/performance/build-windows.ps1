$ErrorActionPreference='Stop'
$repo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$cargo=Join-Path $env:USERPROFILE '.cargo/bin/cargo.exe'
if(-not (Test-Path -LiteralPath $cargo)){throw 'Install Rust before building'}
$oldPath=$env:PATH
$oldConfig=$env:TAURI_CONFIG
Push-Location -LiteralPath $repo
try {
  $env:PATH=([IO.Path]::GetDirectoryName($cargo))+';'+$env:PATH
  & npm.cmd run build
  if($LASTEXITCODE -ne 0){throw 'Frontend build failed'}
  $env:TAURI_CONFIG=Get-Content -LiteralPath (Join-Path $repo 'src-tauri/tauri.performance.conf.json') -Raw
  # custom-protocol embeds the built frontend; --release alone still allows devUrl.
  & $cargo build --locked --manifest-path (Join-Path $repo 'src-tauri/Cargo.toml') --release --features 'perf-acceptance,tauri/custom-protocol' --bin cuenote
  if($LASTEXITCODE -ne 0){throw 'Optimized desktop build failed'}
  $exe=Join-Path $repo 'src-tauri/target/release/cuenote.exe'
  [ordered]@{exe=$exe;sha256=(Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant();variant='perf-acceptance';profile='release';embeddedFrontend=$true} | ConvertTo-Json
}finally {
  Pop-Location
  $env:PATH=$oldPath
  if($null -eq $oldConfig){Remove-Item Env:TAURI_CONFIG -ErrorAction SilentlyContinue}else{$env:TAURI_CONFIG=$oldConfig}
}
