#!/usr/bin/env python3
"""Dev server for the web build.

    python3 tools/serve.py [port]        # from the repo root; default 8000

`python -m http.server` with one difference: every response carries
`Cache-Control: no-cache`, so the browser revalidates each file against the
server (If-Modified-Since → 304 when unchanged) instead of applying heuristic
freshness. Without it a `pkg/*.wasm` or `assets/shaders/*.wesl` cached days
ago can be served next to a freshly fetched counterpart, and the mismatch
shows up as a bind-group or pipeline validation error at startup.
`index.html` protects the engine files itself (it keys them on the wasm's
Last-Modified); this covers everything bevy fetches on its own.
"""
import sys
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer


class NoCacheHandler(SimpleHTTPRequestHandler):
    extensions_map = {
        **SimpleHTTPRequestHandler.extensions_map,
        ".wasm": "application/wasm",
        ".wesl": "text/plain",
        ".wgsl": "text/plain",
        ".mjs": "text/javascript",
    }

    def end_headers(self):
        self.send_header("Cache-Control", "no-cache")
        super().end_headers()


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
    handler = partial(NoCacheHandler, directory=".")
    with ThreadingHTTPServer(("", port), handler) as httpd:
        print(f"Serving the web build on http://localhost:{port}/ (Cache-Control: no-cache)")
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
