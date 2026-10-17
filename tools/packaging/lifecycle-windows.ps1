param(
  [Parameter(Mandatory)][ValidateSet('install','uninstall','start')][string]$Action,
  [Parameter(Mandatory)][string]$Path,
  [string]$ExpectedVersion
)
$ErrorActionPreference='Stop'
if($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
  throw 'Installer lifecycle tests run only on disposable GitHub-hosted runners'
}
if($Action -eq 'install') {
  $process=Start-Process -FilePath $Path -ArgumentList @('/S','/NS') -PassThru -WindowStyle Hidden
  if(-not $process.WaitForExit(180000)){throw 'Installer timed out'}
  if($process.ExitCode -ne 0){throw "Install failed: $($process.ExitCode)"}
  $registered=Get-ItemProperty -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\CueNote'
  if($registered.DisplayVersion -ne $ExpectedVersion){throw 'Installed version mismatch'}
} elseif($Action -eq 'uninstall') {
  $process=Start-Process -FilePath $Path -ArgumentList '/S' -PassThru -WindowStyle Hidden
  if(-not $process.WaitForExit(120000)){throw 'Uninstaller timed out'}
  if($process.ExitCode -ne 0){throw "Uninstall failed: $($process.ExitCode)"}
  $deadline=[DateTime]::UtcNow.AddSeconds(60)
  while((Test-Path -LiteralPath (Join-Path (Split-Path $Path) 'cuenote.exe')) -and [DateTime]::UtcNow -lt $deadline){Start-Sleep -Milliseconds 250}
  if(Test-Path -LiteralPath (Join-Path (Split-Path $Path) 'cuenote.exe')){throw 'Application still installed'}
} else {
  Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class CueNoteWindowProbe {
  public delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr arg);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, System.Text.StringBuilder text, int count);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
  public static IntPtr Find(uint pid) {
    IntPtr found=IntPtr.Zero;
    EnumWindows((hwnd,arg) => {
      uint owner; GetWindowThreadProcessId(hwnd,out owner);
      var title=new System.Text.StringBuilder(256); GetWindowText(hwnd,title,256);
      if(owner==pid && title.ToString()=="CueNote") { found=hwnd; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
'@
  $process=Start-Process -FilePath $Path -PassThru -WindowStyle Hidden
  try {
    $deadline=[DateTime]::UtcNow.AddSeconds(45)
    do {
      Start-Sleep -Milliseconds 250
      $process.Refresh()
      if($process.HasExited){throw "Application exited early: $($process.ExitCode)"}
      $appWindow=[CueNoteWindowProbe]::Find($process.Id)
    } while($appWindow -eq [IntPtr]::Zero -and [DateTime]::UtcNow -lt $deadline)
    if($appWindow -eq [IntPtr]::Zero){throw 'No application window appeared'}
    Start-Sleep -Seconds 5
    if($process.HasExited){throw 'Application failed after startup'}
    if(-not [CueNoteWindowProbe]::PostMessage($appWindow,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)){throw 'Cannot request application close'}
    if(-not $process.WaitForExit(30000)){throw 'Application did not close normally'}
  } finally {
    if(-not $process.HasExited){Stop-Process -Id $process.Id -Force}
  }
}
