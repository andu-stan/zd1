#!/usr/bin/env bash
# Build our MCU firmware and flash it without touching the speaker: `nix build .#mcu`, then `zd1 flash` (into DFU
# through the host link, a backup of what is on it into units/<unit>/mcu/dfu-backups/, flash, verify, start, then the
# new firmware's version and boot log).
# usage: ./flash.sh [--no-build] [IMAGE] [--unit DIR]     default image: result/zd1.hex (linked here by the build)
set -euo pipefail
cd "$(dirname "$0")"
if [ "${1:-}" = "--no-build" ]; then shift; else nix build -o result ..#mcu; fi
exec nix develop .. -c zd1 flash "$@"
