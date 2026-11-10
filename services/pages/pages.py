"""Web pages as text for Ranunculus's read_page tool: webpage.py behind a small HTTP API. Standard library only.

    python pages.py [--host 0.0.0.0] [--port 8000]

POST /read    {"url": "...", "part": 1} -> {"ok": true, "title", "site", "part", "parts", "text"}
                                           or {"ok": false, "error": "..."}
GET  /health  -> {"engine": "webpage"}"""

import argparse
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from webpage import read_page


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--host', default='0.0.0.0')
    ap.add_argument('--port', type=int, default=8000)
    a = ap.parse_args()
    one = threading.Lock()  # webpage.CACHE is a plain OrderedDict

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def send(self, code, obj):
            body = json.dumps(obj, ensure_ascii=False).encode()
            self.send_response(code)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path == '/health':
                return self.send(200, {'engine': 'webpage'})
            self.send(404, {})

        def do_POST(self):
            if self.path != '/read':
                return self.send(404, {})
            try:
                req = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or b'{}')
            except ValueError as e:
                return self.send(400, {'ok': False, 'error': str(e)})
            t = time.time()
            with one:
                try:
                    r = read_page(req.get('url', ''), req.get('part') or 1)
                except Exception as e:  # noqa: BLE001 — the model gets the error text back
                    r = {'ok': False, 'error': str(e)}
            print(
                time.strftime('%H:%M:%S'),
                f'{time.time() - t:4.2f} s {req.get("url")!r} part {req.get("part") or 1}: '
                + (f'{r.get("parts")} parts' if r.get('ok') else r.get('error', '')),
                flush=True,
            )
            self.send(200, r)

    print(time.strftime('%H:%M:%S'), f'pages on {a.host}:{a.port}', flush=True)
    ThreadingHTTPServer((a.host, a.port), Handler).serve_forever()


if __name__ == '__main__':
    main()
