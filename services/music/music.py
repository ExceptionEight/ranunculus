"""Music for Ranunculus: finds it and gives its audio to the server's player. Providers by the track id's prefix:
«ytm:» — YouTube Music (ytm.py). Needs yt-dlp and ffmpeg.

    python music.py [--host 0.0.0.0] [--port 8000]

POST /v1/find   {"query": "IC3PEAK", "provider": "ytm"}   (provider: optional; unknown -> 400)
                -> {"what": "IC3PEAK, песни вперемешку", "tracks": [{"id": "ytm:…", "title", "artist", "duration"}]}
                   (what: null, tracks: [] — nothing found; HTTP 502 {"error"} — the provider failed)
GET  /v1/audio?id=ytm:…&start=12.5
                -> 200, audio/L16 48 kHz stereo (s16le), X-Duration; the body ends with the track (empty: `start` was
                   at its end). A stream that broke is reopened inside: the server gets one continuous stream.
                   502 {"error"} — no audio for this track (a live stream, at once).
GET  /health    -> {"providers": [...]}
The server closing the audio connection (pause, stop, next) stops the decoding at once."""

import argparse
import json
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import ytm


def log(s):
    print(time.strftime('%H:%M:%S'), s, flush=True)


ytm.log = log


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--host', default='0.0.0.0')
    ap.add_argument('--port', type=int, default=8000)
    a = ap.parse_args()
    resolver = ytm.Resolver()

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
            u = urllib.parse.urlparse(self.path)
            if u.path == '/health':
                return self.send(200, {'providers': ['ytm']})
            if u.path == '/v1/audio':
                return self.audio(urllib.parse.parse_qs(u.query))
            self.send(404, {'error': 'no such path'})

        def do_POST(self):
            if self.path != '/v1/find':
                return self.send(404, {'error': 'no such path'})
            try:
                req = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or b'{}')
                query = str(req['query']).strip()
            except (ValueError, KeyError, TypeError):
                return self.send(400, {'error': 'need {"query": ...}'})
            if req.get('provider') not in (None, '', 'ytm'):
                return self.send(400, {'error': f'нет такого музыкального сервиса: «{req["provider"]}»'})
            t = time.time()
            try:
                what, queue = ytm.find(query)
            except Exception as e:  # noqa: BLE001 — the server tells the model, the model tells the user
                log(f'search: {e}')
                return self.send(502, {'error': f'YouTube Music не ответил: {str(e)[:120]}'})
            tracks = [
                {'id': 'ytm:' + vid, 'title': title, 'artist': artist, 'duration': duration}
                for vid, title, artist, duration in queue
            ]
            log(f'search: {len(tracks)} tracks in {time.time() - t:.1f} s')
            self.send(200, {'what': what, 'tracks': tracks})

        def audio(self, query):
            track = (query.get('id') or [''])[0]
            try:
                start = max(0.0, float((query.get('start') or ['0'])[0]))
            except ValueError:
                start = 0.0
            if not track.startswith('ytm:') or len(track) < 6:
                return self.send(400, {'error': f'unknown track id «{track}»'})
            stream = ytm.Stream(resolver, track[4:], start)
            try:
                first = stream.read()  # before the headers: no audio at all is an error the server can see
                if not first and stream.ended_how != 'end':  # (at its end: an empty stream — the track is over)
                    log(f'{track}: {stream.error or "no audio"}')
                    return self.send(502, {'error': stream.error or 'нет звука: YouTube не отдаёт этот трек'})
                self.send_response(200)
                self.send_header('Content-Type', f'audio/L16; rate={ytm.SR}; channels={ytm.CH}')
                if stream.duration:
                    self.send_header('X-Duration', str(stream.duration))
                self.end_headers()
                chunk = first
                while chunk:
                    self.wfile.write(chunk)
                    chunk = stream.read()
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass  # the server closed it: pause, stop, the next track
            finally:
                stream.close()

    log(f'music on {a.host}:{a.port}')
    server = ThreadingHTTPServer((a.host, a.port), Handler)
    server.daemon_threads = True
    server.serve_forever()


if __name__ == '__main__':
    main()
