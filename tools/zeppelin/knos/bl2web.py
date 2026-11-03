#!/usr/bin/env python3
"""Flash a .bcd through the DM870 BL2 web updater (the module must be in BL2: `zd1 module esc`).

Flow, from BL2's own pages (its strings: re/ghidra/setup.py bl2): POST multipart/form-data, file field
`appFirmware`, to /goform/formPostHandler -> BL2 checks the .bcd (CRCs, HWID, version) and shows
bl_confirm_download.asp -> GET /goform/formDownloadConfirmationHandler?button=<Ok> -> BL2 writes NAND;
bl_flash_burn_status.asp reports
"Firmware upload status: N% completed" and "The upload process finished successfully" / "...failed".
Plain mode only ("Update Image & CnE" as the confirm page proposes; the expert page's cnedownload/hwcheckoverride
are not used). The user settings are erased by BL2 like any update.

  usage: python3 -I bl2web.py IP[:PORT] IMAGE.bcd [--yes] [--iface NAME]     (default port 80)

--iface: connect through that interface. With the module on Wi-Fi as well as on the wired dongle, both are on
169.254/16 and the computer may route 169.254.1.1 over Wi-Fi, where BL2 is not: `--iface=en8` (the dongle on a Mac).

WRITES THE MODULE'S APP PARTITION. Without --yes: only fetches the update page (checks BL2 is there).
"""

import argparse
import http.client
import os
import re
import socket
import sys
import time
import urllib.parse

FAILED_GRACE = 120  # s a "failed" status page must persist before it counts (a burn takes ~30-60 s)
OK_BUTTON = "      Ok      "  # value of the confirm page's submit button (aspSetFirmwareUploadConfirmationHandler)


IFACE = None  # --iface
IP_BOUND_IF = 25  # macOS <netinet/in.h>: the socket's traffic goes through this interface only


def bind_to(s, iface):
    if sys.platform == "darwin":
        s.setsockopt(socket.IPPROTO_IP, IP_BOUND_IF, socket.if_nametoindex(iface))
    else:  # Linux (needs CAP_NET_RAW, or root, before 5.7)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, iface.encode())


class BoundHTTPConnection(http.client.HTTPConnection):
    def connect(self):
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        bind_to(s, IFACE)
        s.settimeout(self.timeout)
        s.connect((self.host, self.port))
        self.sock = s


def conn(host, port, timeout=60):
    return (BoundHTTPConnection if IFACE else http.client.HTTPConnection)(host, port, timeout=timeout)


def get(host, port, path):
    c = conn(host, port)
    c.request("GET", path)
    r = c.getresponse()
    body = r.read().decode("latin-1")
    c.close()
    return r.status, dict(r.getheaders()), body


def text(html):
    return re.sub(r"\s+", " ", re.sub(r"<[^>]+>", " ", html)).strip()


def main():
    global IFACE
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("target", metavar="IP[:PORT]")
    ap.add_argument("image", metavar="IMAGE.bcd")
    ap.add_argument("--yes", action="store_true", help="upload and flash (else only check BL2 is there)")
    ap.add_argument("--iface", help="connect through this network interface")
    a = ap.parse_args()
    IFACE = a.iface
    host, _, port = a.target.partition(":")
    port = int(port or 80)
    img = open(a.image, "rb").read()
    st, _, body = get(host, port, "/1000/bl_firmware_update.asp")
    print("BL2 update page: HTTP %d, %s" % (st, "upload form present" if "appFirmware" in body else "NO upload form"))
    if "appFirmware" not in body:
        sys.exit(1)
    if not a.yes:
        sys.exit("dry run. re-run with --yes to upload %s (%d bytes) and flash it." % (a.image, len(img)))
    boundary = "----zd1bl2web%d" % int(time.time())
    head = (
        '--%s\r\nContent-Disposition: form-data; name="appFirmware"; filename="%s"\r\n'
        "Content-Type: application/octet-stream\r\n\r\n" % (boundary, os.path.basename(a.image))
    ).encode()
    tail = (
        '\r\n--%s\r\nContent-Disposition: form-data; name="appFirmware"\r\n\r\nUpload\r\n--%s--\r\n'
        % (boundary, boundary)
    ).encode()
    c = conn(host, port, timeout=600)
    t0 = time.time()
    c.putrequest("POST", "/goform/formPostHandler")
    c.putheader("Content-Type", "multipart/form-data; boundary=%s" % boundary)
    c.putheader("Content-Length", str(len(head) + len(img) + len(tail)))
    c.endheaders()
    c.send(head)
    for i in range(0, len(img), 64 * 1024):
        c.send(img[i : i + 64 * 1024])
        sys.stderr.write("\r  uploaded %d/%d KB" % (min(i + 65536, len(img)) // 1024, len(img) // 1024))
    c.send(tail)
    r = c.getresponse()
    body = r.read().decode("latin-1")
    loc = r.getheader("Location")
    c.close()
    sys.stderr.write("\n")
    print("upload: HTTP %d after %.0f s%s" % (r.status, time.time() - t0, ", redirect " + loc if loc else ""))
    if loc:
        path = urllib.parse.urlparse(loc).path or loc
        st, _, body = get(host, port, path)
    t = text(body)
    print("  BL2 says:", t[:300])
    if "formDownloadConfirmationHandler" not in body:
        sys.exit("no confirmation form: BL2 rejected the image (see above)")
    st, hdr, body = get(
        host, port, "/goform/formDownloadConfirmationHandler?" + urllib.parse.urlencode({"button": OK_BUTTON})
    )
    loc = hdr.get("Location")
    print("confirm: HTTP %d%s" % (st, ", redirect " + loc if loc else ""))
    status = urllib.parse.urlparse(loc).path if loc else "/1000/firmware_update/bl_flash_burn_status.asp"
    last = None
    lost = 0
    failed_since = None
    for _ in range(900):
        time.sleep(2)
        try:
            st, _, body = get(host, port, status)
        except OSError as e:
            print("  status poll failed:", e)
            lost += 1
            if (last == "100" or failed_since) and lost >= 3:  # BL2 resets the board right after a complete burn
                print(
                    "BL2: burn reached 100 %% and the board reset itself (%.0f s): the new image is starting."
                    % (time.time() - t0)
                )
                if failed_since:
                    print(
                        '  (the "failed" page was stale: BL2 keeps the last update\'s status until the new burn '
                        "ends; the reset is the real verdict)"
                    )
                return
            continue
        t = text(body)
        m = re.search(r"Firmware upload status: (\d+)%", t)
        p = m.group(1) if m else "?"
        if p != last:
            print("  %s%% completed" % p)
            last = p
        if "finished successfully" in t:
            print(
                "BL2: upload finished successfully (%.0f s). BL2 resets the module into the new image by itself: "
                "don't power-cycle it (its first boot reinstalls the system partition, ~60 s); `zd1 module start` "
                "attaches once it is up." % (time.time() - t0)
            )
            return
        if "process failed" in t:
            # After a failed update in the same BL2 session the status page still shows that failure while the next
            # burn runs (emulated with the real BL1, 2026-10-22: "100 % / failed" at once, then the board reset and
            # the new image booted). Only a "failed" page that outlives a burn is a failure.
            if failed_since is None:
                failed_since = time.time()
                print(
                    '  BL2 page says "failed" (%.0f s): waiting %d s for a reset in case it is stale'
                    % (time.time() - t0, FAILED_GRACE)
                )
            elif time.time() - failed_since > FAILED_GRACE:
                sys.exit("BL2: upload FAILED: " + t[:300])
        else:
            failed_since = None
    sys.exit("timed out waiting for BL2")


if __name__ == "__main__":
    main()
