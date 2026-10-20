"""Tiny local crates.io sparse-index + download proxy.

This exists only because the current Windows shell cannot use Schannel TLS
(cargo/curl fail with SEC_E_NO_CREDENTIALS) while CPython's OpenSSL stack can.
Run it on 127.0.0.1 and point cargo at it with scripts\cargo-local.ps1.

It is a development bootstrap, not part of the application.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import threading
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT / ".cache" / "crates-proxy"
CACHE.mkdir(parents=True, exist_ok=True)
UPSTREAM_INDEX = "https://index.crates.io"
UPSTREAM_STATIC = "https://static.crates.io/crates"
PORT = int(os.environ.get("HYO_CRATES_PROXY_PORT", "8765"))
UA = "hyo-crates-proxy/0.1 (local development)"


def cache_path(key: str) -> Path:
    digest = hashlib.sha256(key.encode()).hexdigest()
    return CACHE / digest


def fetch(url: str) -> bytes:
    cached = cache_path(url)
    if cached.exists():
        return cached.read_bytes()
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as resp:
        data = resp.read()
    tmp = cached.with_suffix(".tmp")
    tmp.write_bytes(data)
    tmp.replace(cached)
    return data


def index_path(name: str) -> str:
    n = name.lower()
    if len(n) == 1:
        return f"1/{n}"
    if len(n) == 2:
        return f"2/{n}"
    if len(n) == 3:
        return f"3/{n[0]}/{n}"
    return f"{n[:2]}/{n[2:4]}/{n}"


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # quieter cargo output
        sys.stderr.write("[crates-proxy] " + (fmt % args) + "\n")

    def send_bytes(self, data: bytes, ctype="application/octet-stream"):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "max-age=300")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        try:
            if path == "/config.json":
                body = json.dumps(
                    {
                        "dl": f"http://127.0.0.1:{PORT}/crate",
                        "api": f"http://127.0.0.1:{PORT}/api",
                    }
                ).encode()
                self.send_bytes(body, "application/json")
                return
            if path == "/health":
                self.send_bytes(b"ok", "text/plain")
                return
            if path.startswith("/crate/"):
                # /crate/{crate}/{version}/download
                parts = [p for p in path.split("/") if p]
                if len(parts) < 4:
                    self.send_error(400, "bad download path")
                    return
                name, version = parts[1], parts[2]
                url = f"{UPSTREAM_STATIC}/{name}/{name}-{version}.crate"
                self.send_bytes(fetch(url))
                return
            if path.startswith("/api/"):
                # crates.io API passthrough (rarely needed, but harmless)
                url = "https://crates.io" + path
                self.send_bytes(fetch(url), "application/json")
                return
            name = path.strip("/").split("/")[-1]
            if not name or "/" in name:
                self.send_error(404)
                return
            url = f"{UPSTREAM_INDEX}/{index_path(name)}"
            self.send_bytes(fetch(url), "text/plain; charset=utf-8")
        except urllib.error.HTTPError as e:
            self.send_error(e.code, e.reason)
        except Exception as e:  # noqa: BLE001 - local diagnostic proxy
            sys.stderr.write(f"[crates-proxy] ERROR {path}: {e}\n")
            self.send_error(502, str(e))


def main():
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"crates proxy listening on http://127.0.0.1:{PORT}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
