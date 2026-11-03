#!/bin/sh
# Keep what the work needs through a `nix store gc`: the images zemu and mcu/flash.sh use (the module's, the MCU's,
# the test unit) and the dev shells with their toolchains, as links in emu/img/roots/ (gitignored). Everything else
# in the store goes at the next gc and is built again (or fetched) when needed.
#   tools/zeppelin/gcroots.sh && nix store gc
set -e
cd "$(dirname "$0")/../.."
r=emu/img/roots
rm -rf "$r" && mkdir -p "$r"
for p in os-image test-unit mcu mcu-emu llvm; do nix build ".#$p" -o "$r/$p"; done
for s in default cross; do nix develop ".#$s" --profile "$r/shell-$s" -c true; done
ls -l "$r"
