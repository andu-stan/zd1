import os
import sys

if not __package__:  # `python3 tools/zeppelin ...`
    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from zeppelin.cli import main  # noqa: E402

main()
