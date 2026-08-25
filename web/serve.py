#!/usr/bin/env python3
"""A static server that sends COOP and COEP, and nothing else.

It exists because the interesting failure is a *missing* header, and testing
that with a server which cannot be told to omit them proves nothing. Run it
with --no-isolation to serve the same files without the headers and watch the
loader say so.

Not a production server. `python3 -m http.server` with two extra headers.
"""

import argparse
import functools
import http.server
import os


class Handler(http.server.SimpleHTTPRequestHandler):
    isolate = True

    def end_headers(self):
        if self.isolate:
            # Together these are what makes `SharedArrayBuffer` available, and
            # therefore what makes the threaded variant reachable at all.
            self.send_header("Cross-Origin-Opener-Policy", "same-origin")
            self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, *args):
        pass


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", type=int, default=8080)
    ap.add_argument(
        "--no-isolation",
        action="store_true",
        help="omit COOP/COEP, so the loader has to degrade visibly",
    )
    args = ap.parse_args()

    handler = functools.partial(
        type("H", (Handler,), {"isolate": not args.no_isolation}),
        directory=os.path.dirname(os.path.abspath(__file__)),
    )
    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), handler)
    # The *bound* port, not the requested one: --port 0 is how a test gets a
    # free one, and printing what was asked for instead of what was given sends
    # the caller to port zero.
    port = server.server_address[1]
    print(f"http://127.0.0.1:{port}/  isolated={not args.no_isolation}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
