#!/bin/bash
set -euo pipefail
cd -- "$(dirname -- "$0")"
kit="$(pwd -P)"
result="$HOME/CueNote-acceptance-$(date +%Y%m%d-%H%M%S)"
printf '%s\n' 'CueNote：先读取 README.md。测量期间请勿操作键盘鼠标或切换窗口。'
printf '%s\n' "结果目录：$result"
"$kit/cuenote-performance-driver" doctor
"$kit/cuenote-performance-driver" matrix "$kit" "$result"
printf '%s\n' '已完成。请保留结果目录；summary.json 只是计时结果，不代表全部双端验收通过。'
