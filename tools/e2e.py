"""One conversation through a running Ranunculus, end to end, with no station and no real model:

- a stand-in station speaks the protocol (crates/proto): the wake, the command's audio as a microphone gives it,
  and it records what the server plays back;
- a stand-in model answers with a fixed phrase over the OpenAI chat API;
- the server, speech recognition and the voice are the real ones.

    docker compose -f compose.yaml -f tools/e2e.compose.yaml up -d
    uv run --with numpy --with websockets tools/e2e.py --token "$(docker compose exec -T server cat data/token)"
        [--web https://127.0.0.1:52702] [--stations wss://127.0.0.1:52700/station] [--tts http://127.0.0.1:47711]
        [--model-host host.docker.internal] [--model-port 47720]

Needs a fresh server (it makes the first user) or the test user it made before, and the voice service reachable
from here (--tts): it says the command. Exit code 0: the command was heard, answered by the model and spoken, and a
station of another version is given the server's own client."""

import argparse
import asyncio
import hashlib
import http.cookiejar
import json
import ssl
import struct
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import websockets

PROTO = 1
USER, PASSWORD = 'e2e', 'correct horse battery staple'
DEVICE = 'e2e-1'
COMMAND = 'Привет, как дела?'
ANSWER = 'Привет! Всё хорошо.'
MODEL = 'scripted'

TLS = ssl.create_default_context()
TLS.check_hostname, TLS.verify_mode = False, ssl.CERT_NONE  # the server's own certificate: a real station pins it


class Web:
    """The web API as a signed-in browser uses it."""

    def __init__(self, base):
        self.base = base
        self.http = urllib.request.build_opener(
            urllib.request.HTTPSHandler(context=TLS), urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar())
        )

    def call(self, method, path, body=None, raw=False):
        req = urllib.request.Request(
            self.base + path,
            None if body is None else json.dumps(body).encode(),
            method=method,
            headers={'Content-Type': 'application/json', 'Origin': self.base},
        )
        try:
            with self.http.open(req, timeout=120) as r:
                data = r.read()
        except urllib.error.HTTPError as e:
            raise SystemExit(f'{method} {path}: HTTP {e.code} {e.read().decode()[:200]}')
        return data if raw else json.loads(data)


def model(port):
    """A model that says ANSWER to anything: streamed for a request, whole for the warm-up and the notes."""
    asked = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            req = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
            asked.append(req)
            self.send_response(200)
            if not req.get('stream'):
                body = json.dumps(
                    {
                        'choices': [
                            {'message': {'role': 'assistant', 'content': '{"add": [], "update": [], "remove": []}'}}
                        ]
                    }
                ).encode()
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                return self.wfile.write(body)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            for piece in (ANSWER[:8], ANSWER[8:]):
                self.wfile.write(f'data: {json.dumps({"choices": [{"delta": {"content": piece}}]})}\n\n'.encode())
            self.wfile.write(b'data: [DONE]\n\n')

    server = ThreadingHTTPServer(('0.0.0.0', port), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return asked


def resample(x, rate, to=16000):
    """Mono samples at `rate` -> at `to`: a windowed-sinc low-pass under the new Nyquist, then read off by position."""
    if rate == to:
        return x
    k = np.arange(-64, 65)
    cut = 0.45 * to / rate
    taps = 2 * cut * np.sinc(2 * cut * k) * np.hamming(len(k))
    y = np.convolve(x, taps / taps.sum(), 'same')
    return np.interp(np.arange(0, len(y), rate / to), np.arange(len(y)), y)


def spoken(tts, text):
    """`text` in the voice service's voice, as a station's microphone has speech: 16 kHz s16le at about -60 dBFS."""
    req = urllib.request.Request(
        tts + '/v1/audio/speech',
        json.dumps({'input': text, 'response_format': 'pcm'}).encode(),
        {'Content-Type': 'application/json'},
    )
    with urllib.request.urlopen(req, timeout=120) as r:
        rate, pcm = int(r.headers.get('X-Sample-Rate', 24000)), r.read()
    x = resample(np.frombuffer(pcm, '<i2').astype(np.float64), rate)
    x *= 10 ** (-60 / 20) * 32768 / (np.sqrt((x**2).mean()) + 1e-9)
    return x.astype('<i2')


class Station:
    """A station as the server sees one: it says hello, sends the microphone, takes what is played."""

    def __init__(self, url, token, version):
        self.url, self.token, self.version = url, token, version
        self.t0 = time.monotonic()
        self.streams = {}  # id -> {rate, channels, pcm, first, done}
        self.panel = []  # the panel's states, as they came
        self.listening = False
        self.seq = 0

    def t(self):
        return time.monotonic() - self.t0

    async def connect(self):
        # pings every 5 s, as a station does: the server drops a link silent for 15 s
        self.ws = await websockets.connect(self.url, ssl=TLS, max_size=None, ping_interval=5, ping_timeout=15)
        await self.ws.send(
            json.dumps(
                {
                    't': 'hello',
                    'proto': PROTO,
                    'version': self.version,
                    'device': DEVICE,
                    'token': self.token,
                    'caps': ['mic', 'play', 'kws'],
                }
            )
        )
        welcome = json.loads(await self.ws.recv())
        if welcome.get('t') != 'welcome':
            raise SystemExit(f'the server did not take the station: {welcome}')
        asyncio.create_task(self.receive())

    async def receive(self):
        async for m in self.ws:
            if isinstance(m, bytes):
                if m[0] == 2:  # a stream's audio
                    s = self.streams.get(struct.unpack('<I', m[1:5])[0])
                    if s is not None:
                        s['first'] = s['first'] or self.t()
                        s['pcm'] += m[5:]
                continue
            d = json.loads(m)
            if d['t'] == 'play':
                self.streams[d['id']] = {
                    'rate': d['rate'],
                    'channels': d['channels'],
                    'pcm': bytearray(),
                    'first': None,
                    'done': False,
                }
            elif d['t'] == 'end':
                asyncio.create_task(self.played(d['id']))
            elif d['t'] == 'kill':
                s = self.streams.get(d['id'])
                if s and not s['done']:
                    s['done'] = True
                    await self.ws.send(json.dumps({'t': 'done', 'id': d['id'], 'killed': True}))
            elif d['t'] == 'listen_end':
                self.listening = False
            elif d['t'] == 'volume_get':
                await self.ws.send(json.dumps({'t': 'volume', 'level': 89, 'req': d['req']}))
            elif d['t'] == 'volume_set':
                await self.ws.send(json.dumps({'t': 'volume', 'level': d['level'], 'req': d.get('req')}))
            elif d['t'] == 'panel':
                self.panel.append(d['state'])
            elif d['t'] == 'ping':
                await self.ws.send(json.dumps({'t': 'pong', 'n': d['n']}))

    async def played(self, sid):
        """As a speaker: a stream is done once its audio has played in real time from its first block."""
        s = self.streams[sid]
        left = (s['first'] or self.t()) + len(s['pcm']) / (s['rate'] * s['channels'] * 2) - self.t()
        if left > 0:
            await asyncio.sleep(left)
        if not s['done']:
            s['done'] = True
            await self.ws.send(json.dumps({'t': 'done', 'id': sid, 'killed': False}))

    async def mic(self, x):
        self.seq += 1
        await self.ws.send(b'\x01' + struct.pack('<IQ', self.seq, int(self.t() * 1e6)) + x.astype('<i2').tobytes())

    async def command(self, speech):
        """The wake word heard, then the command as a station streams it: the 2 s before the detection at once (the
        room's noise and the command's first 0.4 s), then 32-ms frames in real time, then the room until listen_end."""
        pre = 32000
        await self.ws.send(json.dumps({'t': 'wake', 'p': 0.97, 'barge': False, 't_us': 0, 'pre': pre}))
        self.listening = True
        noise = (np.random.randn(16000 * 14) * 3).astype('<i2')  # the room: about -80 dBFS
        audio = np.concatenate([noise[:6400], speech, noise[6400:]])
        await self.mic(np.concatenate([noise[-(pre - 6400) :], audio[:6400]]))
        start = time.monotonic()
        for k, i in enumerate(range(6400, len(audio) - 512, 512), 1):
            if not self.listening:
                return
            await self.mic(audio[i : i + 512])
            await asyncio.sleep(max(0.0, start + k * 512 / 16000 - time.monotonic()))
        raise SystemExit('the server did not end the listening in 12 s after the command')


async def offered(url, token):
    """What the server gives a station of another version: (its answer to the hello, the file that follows)."""
    ws = await websockets.connect(url, ssl=TLS, max_size=None)
    await ws.send(
        json.dumps(
            {
                't': 'hello',
                'proto': PROTO,
                'version': '0.0.0-e2e',
                'device': DEVICE + '-old',
                'token': token,
                'caps': [],
            }
        )
    )
    answer, file = json.loads(await ws.recv()), bytearray()
    try:
        async for m in ws:
            if isinstance(m, bytes):
                file += m
    except websockets.ConnectionClosed:
        pass
    return answer, bytes(file)


def loud_part(pcm):
    """dBFS of the loud 32-ms frames of 24 kHz speech (as the server's loudness.rs measures it)."""
    x = np.frombuffer(bytes(pcm), '<i2').astype(np.float64) / 32768
    frames = x[: len(x) // 768 * 768].reshape(-1, 768)
    power = (frames**2).mean(1) + 1e-12
    return 10 * np.log10(power[power > power.max() / 1000].mean())


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--web', default='https://127.0.0.1:52702')
    ap.add_argument('--stations', default='wss://127.0.0.1:52700/station')
    ap.add_argument('--tts', default='http://127.0.0.1:47711')
    ap.add_argument('--token', required=True)
    ap.add_argument('--model-host', default='host.docker.internal', help='this machine as the server reaches it')
    ap.add_argument('--model-port', type=int, default=47720)
    a = ap.parse_args()
    failed = []

    def check(ok, what):
        print('ok  ' if ok else 'FAIL', what)
        if not ok:
            failed.append(what)

    web = Web(a.web)
    auth = web.call('GET', '/api/auth')
    web.call(
        'POST', '/api/auth/register' if auth.get('setup') else '/api/auth/login', {'name': USER, 'password': PASSWORD}
    )
    version = web.call('GET', '/api/info')['version']

    asked = model(a.model_port)
    url = f'http://{a.model_host}:{a.model_port}/v1'
    providers = web.call('GET', '/api/providers')
    mine = next((p for p in providers['items'] if p['kind'] == 'llm' and p['url'] == url), None) or web.call(
        'POST', '/api/providers', {'kind': 'llm', 'name': 'e2e', 'url': url, 'model': MODEL}
    )
    web.call('PATCH', '/api/providers/in-use', {'llm': mine['id']})
    for kind in ('asr', 'tts'):
        check(
            web.call('GET', '/api/providers')['in_use'][kind] is not None,
            f'a {kind} service is in use from the first start',
        )

    speech = spoken(a.tts, COMMAND)
    station = Station(a.stations, a.token, version)
    await station.connect()
    check(
        any(d['id'] == DEVICE and d['online'] for d in web.call('GET', '/api/devices')),
        'the station is online in the web UI',
    )
    before = web.call('GET', f'/api/exchanges?device={DEVICE}&limit=1')['items']
    await station.command(speech)

    exchange = None
    for _ in range(120):
        await asyncio.sleep(0.5)
        items = web.call('GET', f'/api/exchanges?device={DEVICE}&limit=1')['items']
        if items and items != before:
            exchange = items[0]
            break
    check(exchange is not None, 'the conversation is in the chats')
    if exchange:
        heard = ''.join(c for c in exchange['text'].lower() if c.isalpha() or c == ' ')
        check('привет' in heard and 'дела' in heard, f'recognition heard the command: {exchange["text"]!r}')
        check(
            exchange['answer'] == ANSWER and exchange.get('model') == MODEL,
            f'the model in use answered: {exchange["answer"]!r} by {exchange.get("model")!r}',
        )
        check(any(r.get('stream') for r in asked), 'the request reached the model')
    await asyncio.sleep(1.0)
    voice = [s for s in station.streams.values() if s['rate'] == 24000 and len(s['pcm']) > 24000]
    check(
        bool(voice),
        f'the answer was spoken on the station ({sum(len(s["pcm"]) for s in voice) / 48000:.1f} s of voice)',
    )
    if voice:
        level = loud_part(voice[-1]['pcm'])
        check(-17.5 < level < -11.5, f"at the station's level: {level:.1f} dBFS")
    check('listen' in station.panel, f'the panel followed the dialog: {station.panel}')
    answer, file = await offered(a.stations, a.token)
    check(
        answer.get('t') == 'update' and answer.get('version') == version and answer.get('size') == len(file),
        f"a station of another version is given the server's client: {answer.get('t')}, {len(file)} bytes",
    )
    check(
        hashlib.sha256(file).hexdigest() == answer.get('sha256')
        and file[:4] == b'\x7fELF'
        and file[18:20] == b'\xb7\x00',
        'the client matches its SHA-256 and is a program for a station (aarch64)',
    )
    print('passed' if not failed else f'{len(failed)} failed')
    sys.exit(1 if failed else 0)


if __name__ == '__main__':
    asyncio.run(main())
