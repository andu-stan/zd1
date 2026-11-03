#!/usr/bin/env bash
# Rebuild the telnet hook that patch.py splices into the stock firmware from its AVR32 source (telnet_hook.S,
# hook.ld, linked at the cave 0x80038100): `nix build .#telnet-hook` (the AVR32 toolchain on an x86_64 Linux builder),
# copied here. The blob is committed, so patching itself (patch.py) needs only python3.
set -euo pipefail
cd "$(dirname "$0")"
ROOT="$(cd .. && pwd)"
res=$(nix build --no-link --print-out-paths "$ROOT#telnet-hook")
rm -f telnet_hook.bin
cp "$res/telnet_hook.bin" .
chmod 644 telnet_hook.bin
git -C "$ROOT" diff --stat -- telnet_hook.bin
echo "telnet hook rebuilt (no diff above = identical to the committed one)"
