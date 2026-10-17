#!/bin/sh
# Read-only device evidence. Uses only tools included with macOS.
set -eu
if [ "$(uname -s)" != Darwin ]; then
  printf '%s\n' 'This collector must run on the target Mac.' >&2
  exit 1
fi
app=${1:-/Applications/CueNote.app}
printf 'Collected UTC: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
/usr/bin/sw_vers
printf 'Architecture: %s\n' "$(uname -m)"
printf 'CPU: %s\n' "$(/usr/sbin/sysctl -n machdep.cpu.brand_string)"
printf 'Memory bytes: %s\n' "$(/usr/sbin/sysctl -n hw.memsize)"
printf 'Logical CPUs: %s\n' "$(/usr/sbin/sysctl -n hw.logicalcpu)"
webkit=/System/Library/Frameworks/WebKit.framework/Resources/Info.plist
if [ -f "$webkit" ]; then
  printf 'System WebKit build: '
  /usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$webkit"
fi
if [ -f /Applications/OneDrive.app/Contents/Info.plist ]; then
  printf 'OneDrive version: '
  /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' /Applications/OneDrive.app/Contents/Info.plist
else
  printf '%s\n' 'OneDrive version: not found at /Applications/OneDrive.app'
fi
if [ ! -f "$app/Contents/MacOS/cuenote" ]; then
  printf '%s\n' 'CueNote: not installed at the requested path; pass the actual .app path as argument.'
  exit 0
fi
printf 'CueNote version: '
/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist"
printf 'CueNote identifier: '
/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Contents/Info.plist"
printf 'CueNote binary SHA-256: '
/usr/bin/shasum -a 256 "$app/Contents/MacOS/cuenote" | /usr/bin/awk '{print $1}'
/usr/bin/file "$app/Contents/MacOS/cuenote"
if /usr/bin/codesign --verify --deep --strict "$app"; then
  printf '%s\n' 'Code signature integrity: passed (not a notarization or Gatekeeper result)'
else
  printf '%s\n' 'Code signature integrity: FAILED'
  exit 1
fi
