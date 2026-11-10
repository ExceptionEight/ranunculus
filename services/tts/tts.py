"""A voice for Ranunculus: Silero TTS v5 (Russian) on the CPU, behind the OpenAI speech API.

    python tts.py [--host 0.0.0.0] [--port 8000] [--threads 4] [--models /models] [--voice xenia]

POST /v1/audio/speech  {"input": "...", "voice": "xenia", "speed": 1.0, "response_format": "pcm" | "wav"}
                       -> s16le mono 24 kHz (X-Sample-Rate), or a wav
GET  /health           -> {"engine": "silero v5_6_ru", "sr": 24000, "voices": [...]}
Silero skips digits and Latin letters: the text is put into words first (normalize.py). `speed` is one of Silero's
five rates, the nearest. A text with nothing to say gives a moment of silence.
The model (about 140 MB, https://models.silero.ai, CC BY-NC-SA 4.0: not for commercial use) is downloaded into
--models at the first start."""

import argparse
import html
import io
import json
import os
import re
import threading
import time
import urllib.request
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import torch
from normalize import normalize

NAME = 'v5_6_ru'
URL = f'https://models.silero.ai/models/tts/ru/{NAME}.pt'
SR = 24000
PIECE = 800  # characters Silero takes at once, with room to spare
# Silero's prosody rates by the speed they stand for
RATES = [(0.6, 'x-slow'), (0.8, 'slow'), (1.0, None), (1.25, 'fast'), (1.6, 'x-fast')]


def fetch(models):
    """The model's file in `models`, downloaded once."""
    path = os.path.join(models, f'{NAME}.pt')
    if not os.path.exists(path):
        os.makedirs(models, exist_ok=True)
        print(time.strftime('%H:%M:%S'), f'downloading {URL}', flush=True)
        urllib.request.urlretrieve(URL, path + '.part')
        os.replace(path + '.part', path)
    return path


def pieces(text, size=PIECE):
    """The text in pieces the model takes: at sentence ends, a long sentence at commas, a long clause at spaces."""
    out = []
    for s in (x for x in re.split(r'(?<=[.!?…])\s+', text) if x):
        while len(s) > size:
            k = s.rfind(', ', 30, size)
            k = k + 1 if k > 0 else (s.rfind(' ', 30, size) if s.rfind(' ', 30, size) > 0 else size)
            out.append(s[:k].strip())
            s = s[k:].strip()
        if s:
            out.append(s)
    return out


def rate(speed):
    return min(RATES, key=lambda r: abs(r[0] - speed))[1]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--host', default='0.0.0.0')
    ap.add_argument('--port', type=int, default=8000)
    ap.add_argument('--threads', type=int, default=4)
    ap.add_argument('--models', default='/models')
    ap.add_argument('--voice', default='xenia', help='the voice of a request that names none')
    a = ap.parse_args()
    torch.set_num_threads(a.threads)
    model = torch.package.PackageImporter(fetch(a.models)).load_pickle('tts_models', 'model')
    voices = list(model.speakers)
    if a.voice not in voices:
        raise SystemExit(f'--voice {a.voice}: one of {", ".join(voices)}')
    one = threading.Lock()  # the model takes one request at a time

    def say(text, voice, speed):
        """Float samples of `text`, already in words."""
        prosody = rate(speed)
        out = []
        for piece in pieces(text):
            if not re.search(r'[^\W\d_]', piece):  # no letters: Silero refuses it
                continue
            if prosody:
                ssml = f'<speak><prosody rate="{prosody}">{html.escape(piece)}</prosody></speak>'
                out.append(model.apply_tts(ssml_text=ssml, speaker=voice, sample_rate=SR).numpy())
            else:
                out.append(model.apply_tts(text=piece, speaker=voice, sample_rate=SR).numpy())
        return np.concatenate(out) if out else np.zeros(SR // 10, np.float32)

    with one:
        say(normalize('Проверка связи, 1 2 3.'), a.voice, 1.0)  # the first call is slow: not the user's

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def send(self, code, body, ctype='application/json', headers=None):
            self.send_response(code)
            self.send_header('Content-Type', ctype)
            self.send_header('Content-Length', str(len(body)))
            for k, v in (headers or {}).items():
                self.send_header(k, v)
            self.end_headers()
            self.wfile.write(body)

        def error(self, code, message):
            self.send(code, json.dumps({'error': {'message': message}}, ensure_ascii=False).encode())

        def do_GET(self):
            if self.path == '/health':
                return self.send(
                    200, json.dumps({'engine': f'silero {NAME}', 'sr': SR, 'voices': voices, 'voice': a.voice}).encode()
                )
            self.error(404, 'no such path')

        def do_POST(self):
            if self.path != '/v1/audio/speech':
                return self.error(404, 'no such path')
            try:
                req = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
                text, voice, speed = str(req['input']), req.get('voice') or a.voice, float(req.get('speed') or 1.0)
            except (ValueError, KeyError, TypeError):
                return self.error(400, 'need {"input": "..."}')
            if voice not in voices:
                return self.error(400, f'voice «{voice}»: one of {", ".join(voices)}')
            t = time.time()
            try:
                with one:
                    y = say(normalize(text), voice, speed)
            except Exception as e:  # noqa: BLE001 — whatever the model could not say: the caller is told
                return self.error(500, str(e)[:200])
            pcm = (np.clip(y, -1, 1) * 32767).astype('<i2').tobytes()
            headers = {'X-Sample-Rate': str(SR), 'X-Gen-Seconds': f'{time.time() - t:.3f}'}
            if req.get('response_format', 'pcm') != 'wav':
                return self.send(200, pcm, 'audio/pcm', headers)
            buf = io.BytesIO()
            with wave.open(buf, 'wb') as w:
                w.setnchannels(1)
                w.setsampwidth(2)
                w.setframerate(SR)
                w.writeframes(pcm)
            self.send(200, buf.getvalue(), 'audio/wav', headers)

    print(time.strftime('%H:%M:%S'), f'voice on {a.host}:{a.port} (silero {NAME}, {a.voice})', flush=True)
    ThreadingHTTPServer((a.host, a.port), Handler).serve_forever()


if __name__ == '__main__':
    main()
