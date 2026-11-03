"""Repo paths."""

import os

REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
EMU = os.path.join(REPO, "emu")
TOOLS = os.path.join(EMU, "tools")  # trace decoders, gdbtrace, ... (emu/README.md)
IMG = os.path.join(EMU, "img")  # prepared images (gitignored)
RUNS = os.path.join(EMU, "run")  # session output (gitignored)
