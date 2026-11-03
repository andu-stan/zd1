"""The `zd1` command (host/cli: the speaker over USB, host/core's flows) for Python tools: the one client of the host
link, so nothing here talks to the speaker itself.

  import zd1
  zd1.output('module', 'hold')      a command's output (RuntimeError with its error if it fails)
  zd1.run_on_module('uname -a')     a shell command on the module's console (`zd1 console run`): its output
"""

import os
import shutil
import subprocess

REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))


def argv():
    """How to run zd1: $ZD1, the `zd1` on the PATH (`nix develop`), or cargo from the repo (built as needed)."""
    if os.environ.get("ZD1"):
        return [os.environ["ZD1"]]
    if shutil.which("zd1"):
        return ["zd1"]
    return [
        "cargo",
        "run",
        "-q",
        "--release",
        "--manifest-path",
        os.path.join(REPO, "Cargo.toml"),
        "-p",
        "zd1-cli",
        "--",
    ]


def output(*args):
    r = subprocess.run(argv() + [str(a) for a in args], capture_output=True, text=True)
    if r.returncode:
        raise RuntimeError("zd1 %s: %s" % (" ".join(map(str, args)), r.stderr.strip()))
    return r.stdout


def run_on_module(cmd, timeout=30):
    """A shell command on the module's console, to its end (`zd1 console run`): its output, whatever its status."""
    r = subprocess.run(argv() + ["console", "run", "-t", str(int(timeout)), cmd], capture_output=True, text=True)
    if r.returncode and not r.stdout and r.stderr:
        raise RuntimeError("zd1 console run: %s" % r.stderr.strip())
    return r.stdout.rstrip("\n")
