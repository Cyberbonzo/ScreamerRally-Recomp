#!/usr/bin/env bash
set -eu
cd "$(dirname "$(realpath "$0")")"
mkdir -p "$PWD/logs"
export STEAM_COMPAT_CLIENT_INSTALL_PATH="${STEAM_COMPAT_CLIENT_INSTALL_PATH:-$HOME/.local/share/Steam}"
export STEAM_COMPAT_DATA_PATH="$PWD/.proton" DXVK_FRAME_RATE=60 DXVK_LOG_PATH="$PWD/logs"
unset LD_PRELOAD
export PROTON_USE_WOW64=1
export SR_COOPERATIVE_TIMER=1
# The game uses exceptions for normal DOS services. Per-exception logging stalls I/O.
export PROTON_LOG=0 WINEDEBUG=-all
common="$STEAM_COMPAT_CLIENT_INSTALL_PATH/steamapps/common"
exec "$common/SteamLinuxRuntime_4/_v2-entry-point" --verb=waitforexitandrun -- "$common/Proton - Experimental/proton" waitforexitandrun "$PWD/ScreamerRally-Ally.exe" >> "$PWD/logs/proton.log" 2>&1
