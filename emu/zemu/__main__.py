import os
import sys

if not __package__:  # `python3 emu/zemu ...`
    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from zemu.cli import main  # noqa: E402

main()
