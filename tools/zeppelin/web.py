#!/usr/bin/env python3
"""The web page (host/web: the updater and settings over WebUSB) on this computer: `nix build .#web` (its store path,
no link), served on localhost (WebUSB wants a secure origin: localhost counts), with this checkout's built images
under images/ as a release manifest of one release (images/index.json: what the hosted page gets from its releases,
docs/web.md, Releases; each image described by `zd1 image`). The images are the ones `zd1 install` takes by default:
mcu/result (mcu/flash.sh) and emu/img/os (`nix build -o emu/img/os .#os-image`), those that exist.

  zeppelin web [--port 8765]     then open http://localhost:8765 in Chrome, Edge or Brave
"""

import argparse
import datetime
import functools
import http.server
import json
import os
import shutil
import subprocess
import sys

REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
IMAGES = {
    "system": os.path.join(REPO, "emu", "img", "os", "system.cpio.gz"),  # nix build -o emu/img/os .#os-image
    "mcu": os.path.join(REPO, "mcu", "result", "zd1.hex"),  # mcu/flash.sh, nix build .#mcu
    "bcd": os.path.join(REPO, "emu", "img", "os", "os.bcd"),
}


def manifest():
    """One release: this checkout's built images (the ones that exist), described by `zd1 image` (on the PATH in
    `nix develop`, else through nix)."""
    paths = [p for p in IMAGES.values() if os.path.exists(p)]
    if not paths:
        return {"releases": []}
    zd1 = [shutil.which("zd1")] if shutil.which("zd1") else ["nix", "run", ".#zd1", "--"]
    out = subprocess.run(zd1 + ["image"] + paths, cwd=REPO, check=True, capture_output=True, text=True).stdout
    rel = {
        "version": "this computer's build",
        "date": datetime.date.today().isoformat(),
        "notes": "the images built in this checkout (emu/img/os, mcu/result)",
    }
    for line in out.splitlines():
        j = json.loads(line)
        rel[j.pop("kind")] = j
    if "system" in rel:
        rel["version"] = rel["system"]["version"]
    return {"releases": [rel]}


class Handler(http.server.SimpleHTTPRequestHandler):
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".wasm": "application/wasm",
        ".js": "text/javascript",
    }

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/images/index.json":
            return self.send_bytes(json.dumps(manifest()).encode(), "application/json")
        for p in IMAGES.values():
            if path == "/images/" + os.path.basename(p) and os.path.exists(p):
                return self.send_bytes(open(p, "rb").read(), "application/octet-stream")
        return super().do_GET()

    def send_bytes(self, data, ctype):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def send_head(self):
        # the page's files are in the nix store, every one dated 1970: a browser's If-Modified-Since would get 304s
        # for a rebuilt page forever. Nothing here is cached.
        del self.headers["If-Modified-Since"]
        return super().send_head()

    def end_headers(self):
        if not self._headers_buffer or b"Cache-Control" not in b"".join(self._headers_buffer):
            self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, *a):
        pass


def main(argv=None):
    ap = argparse.ArgumentParser(
        prog="zeppelin web", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--port", type=int, default=8765)
    a = ap.parse_args(argv)
    build = ["nix", "build", "--no-link", "--print-out-paths", ".#web"]
    page = subprocess.run(build, cwd=REPO, check=True, capture_output=True, text=True).stdout.strip()
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", a.port), functools.partial(Handler, directory=page))
    print("http://localhost:%d  (Ctrl-C ends)" % a.port)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
