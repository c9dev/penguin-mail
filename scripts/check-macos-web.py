#!/usr/bin/env python3
"""Check WKWebView against a local server, without reaching a mail account."""

import http.server
import os
from pathlib import Path
import subprocess
import tempfile
import threading

root = Path(__file__).resolve().parent.parent
fixture = (root / "app/tests/fixtures/hidden-images.html").read_bytes()
requests = []


class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        requests.append(self.path)
        self.send_response(200)
        self.send_header("Content-Type", "text/html" if self.path == "/" else "image/png")
        self.end_headers()
        self.wfile.write(fixture if self.path == "/" else b"")

    def log_message(self, *args):
        pass


with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Page) as server:
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        with tempfile.TemporaryDirectory() as sandbox:
            folder = Path(sandbox)
            (folder / "gnupg").mkdir(mode=0o700)
            script = folder / "steps.txt"
            script.write_text(f"check-web http://127.0.0.1:{server.server_port}/\nquit\n")
            env = os.environ | {"GNUPGHOME": str(folder / "gnupg"),
                                "XDG_CACHE_HOME": str(folder / "cache")}
            subprocess.run([str(root / "scripts/drive-macos.sh"), str(script)],
                           env=env, check=True, timeout=150)
    finally:
        server.shutdown()
        worker.join()
assert requests == ["/"], f"The hidden page fetched images: {requests}"
print("Only the HTML was fetched; both inline and background pictures were blocked.")
