param([Parameter(Mandatory=$true)][string]$ScratchBase)
$ErrorActionPreference='Stop'
if (-not [IO.Path]::IsPathRooted($ScratchBase) -or $ScratchBase -match '(^|[\\/])\.\.([\\/]|$)' -or $ScratchBase -notmatch '[\\/]PROTOTYPE-wipe-me$') { throw 'Expected absolute scratch root' }
$probeRoot=Join-Path $ScratchBase ('rename-probe-'+[Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($probeRoot) | Out-Null
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class CueNoteRenameProbe {
 [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
 public static extern bool MoveFileExW(string existing, string destination, uint flags);
 [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
 public static extern bool CreateHardLinkW(string destination, string existing, IntPtr security);
 [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
 public static extern bool ReplaceFileW(string destination, string replacement, IntPtr backup, uint flags, IntPtr exclude, IntPtr reserved);
}
'@
foreach($operation in @('extended-path-move','create-hard-link','replace-existing')) {
 $key=[Guid]::NewGuid().ToString('N')
 $source=Join-Path $probeRoot ($key+'-source.tmp')
 $destination=Join-Path $probeRoot ($key+'-destination.cuenote')
 [IO.File]::WriteAllText($source,'CueNote new value')
 switch($operation) {
  'extended-path-move' { $ok=[CueNoteRenameProbe]::MoveFileExW(('\\?\'+$source),('\\?\'+$destination),0) }
  'create-hard-link' { $ok=[CueNoteRenameProbe]::CreateHardLinkW($destination,$source,[IntPtr]::Zero) }
  'replace-existing' { [IO.File]::WriteAllText($destination,'Previous value'); $ok=[CueNoteRenameProbe]::ReplaceFileW($destination,$source,[IntPtr]::Zero,0,[IntPtr]::Zero,[IntPtr]::Zero) }
 }
 $errorCode=if($ok){0}else{[Runtime.InteropServices.Marshal]::GetLastWin32Error()}
 [pscustomobject]@{Operation=$operation;Succeeded=$ok;Error=$errorCode;Directory=$probeRoot} | ConvertTo-Json -Compress
}
foreach($pair in @(@('.tmp','.tmp'),@('.tmp','.cuenote'),@('.cuenote','.cuenote'),@('.cuenote','.tmp'))) {
 $key=[Guid]::NewGuid().ToString('N')
 $source=Join-Path $probeRoot ($key+'-source'+$pair[0])
 $destination=Join-Path $probeRoot ($key+'-destination'+$pair[1])
 [IO.File]::WriteAllText($source,'CueNote disposable rename probe')
 $attributes=[IO.File]::GetAttributes($source).ToString()
 $ok=[CueNoteRenameProbe]::MoveFileExW($source,$destination,0)
 $errorCode=if($ok){0}else{[Runtime.InteropServices.Marshal]::GetLastWin32Error()}
 [pscustomobject]@{SourceExtension=$pair[0];DestinationExtension=$pair[1];SourceAttributes=$attributes;Succeeded=$ok;Error=$errorCode;Directory=$probeRoot} | ConvertTo-Json -Compress
}
