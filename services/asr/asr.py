"""Speech recognition for Ranunculus: GigaAM v3 (Sber, MIT) on the CPU, behind the OpenAI transcription API.

    python asr.py [--host 0.0.0.0] [--port 8000] [--threads 4] [--models /models]

POST /v1/audio/transcriptions  multipart/form-data: file (wav 16 kHz mono 16-bit, or raw s16le 16 kHz); the other
                               fields are ignored -> {"text": "...", "seconds": 0.21}
GET  /health                   -> {"engine": "gigaam v3_e2e_rnnt", "device": "cpu"}
The model (about 430 MB) is downloaded into --models at the first start, from its authors' repository on Hugging Face
(ai-sage/GigaAM-v3, MIT) at a fixed revision, and checked by SHA-256. A station's microphone signal is quiet (speech
around -60 dBFS): it is brought to a normal level before recognition."""

import argparse
import hashlib
import io
import json
import os
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import soundfile as sf
import torch

MODEL = 'v3_e2e_rnnt'
SR = 16000
# the e2e_rnnt branch of https://huggingface.co/ai-sage/GigaAM-v3: the same weights the gigaam package downloads
# from its own server, as plain tensors (no pickled code)
SOURCE = 'https://huggingface.co/ai-sage/GigaAM-v3/resolve/7655ad717f8122257385bb4b2f373db3697e8680'
FILES = {
    'config.json': '02361ba9cafd6c3ec66fcdd73494c3b562a60eb2a2d1b13f3cb04ae440d93e52',
    'tokenizer.model': '828c12c991019eef952a960661f25a92d6ad279591e2ea466b4aeddf1d20a18a',
    'pytorch_model.bin': 'afc6dcbae8320ea56f2cddebc0f13fbf62c9d59b6ddcad899782623c8610826a',
}
# the repository's config names the classes of its own loader; the gigaam package has the same ones here
CLASSES = {
    'FeatureExtractor': 'gigaam.preprocess',
    'ConformerEncoder': 'gigaam.encoder',
    'RNNTHead': 'gigaam.decoder',
    'RNNTGreedyDecoding': 'gigaam.decoding',
}


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def fetch(models):
    """The model's files in `models`: downloaded once, each checked before it is used."""
    folder = os.path.join(models, MODEL)
    os.makedirs(folder, exist_ok=True)
    for name, digest in FILES.items():
        path = os.path.join(folder, name)
        if not os.path.exists(path):
            print(time.strftime('%H:%M:%S'), f'downloading {SOURCE}/{name}', flush=True)
            urllib.request.urlretrieve(f'{SOURCE}/{name}', path + '.part')
            if sha256(path + '.part') != digest:
                os.remove(path + '.part')
                raise SystemExit(f'{name}: the download does not match its SHA-256')
            os.replace(path + '.part', path)
    return folder


def package_classes(cfg):
    """`_target_` of every part of the model -> the gigaam package's class of that name."""
    if isinstance(cfg, dict):
        if '_target_' in cfg:
            name = cfg['_target_'].rsplit('.', 1)[-1]
            cfg['_target_'] = f'{CLASSES[name]}.{name}'
        for v in cfg.values():
            package_classes(v)


def level(x, target_db=-23.0, peak=0.9):
    """Float samples at a level the model was trained on: `target_db` RMS, peaks under `peak`."""
    x = x.astype(np.float32)
    rms = np.sqrt(np.mean(x**2)) + 1e-9
    gain = min(10 ** (target_db / 20) / rms, peak / (np.abs(x).max() + 1e-9))
    return x * gain


class Giga:
    """GigaAM fed with arrays: its own loader takes files and shells out to ffmpeg."""

    def __init__(self, models, threads):
        from gigaam.model import GigaAMASR
        from omegaconf import OmegaConf

        torch.set_num_threads(threads)
        folder = fetch(models)
        with open(os.path.join(folder, 'config.json'), encoding='utf-8') as f:
            cfg = json.load(f)['cfg']['model']['cfg']
        package_classes(cfg)
        cfg['decoding']['model_path'] = os.path.join(folder, 'tokenizer.model')
        self.model = GigaAMASR(OmegaConf.create(cfg))
        weights = torch.load(os.path.join(folder, 'pytorch_model.bin'), map_location='cpu', weights_only=True)
        self.model.load_state_dict({name.removeprefix('model.'): tensor for name, tensor in weights.items()})
        self.model.eval()

    @torch.inference_mode()
    def __call__(self, x):
        """16 kHz float samples -> (text, seconds spent)."""
        t = time.time()
        wav = torch.tensor(level(x)).unsqueeze(0)
        length = torch.full([1], wav.shape[-1])
        encoded, encoded_len = self.model.forward(wav, length)
        text = self.model._decode(encoded, encoded_len, length, False)[0][0]
        return text, time.time() - t


def multipart(body, ctype):
    """{name: bytes} of a multipart/form-data body (enough for OpenAI-style uploads)."""
    boundary = ctype.split('boundary=', 1)[1].strip().strip('"').encode()
    out = {}
    for part in body.split(b'--' + boundary)[1:]:
        if part.startswith(b'--'):
            break
        head, _, data = part.partition(b'\r\n\r\n')
        for line in head.split(b'\r\n'):
            if line.lower().startswith(b'content-disposition') and b'name="' in line:
                out[line.split(b'name="', 1)[1].split(b'"', 1)[0].decode()] = (
                    data[:-2] if data.endswith(b'\r\n') else data
                )
    return out


def audio(blob):
    """A wav file or raw s16le, both 16 kHz -> mono float samples."""
    if blob[:4] != b'RIFF':
        return np.frombuffer(blob, '<i2').astype(np.float32) / 32768
    x, sr = sf.read(io.BytesIO(blob), dtype='float32')
    if sr != SR:
        raise ValueError(f'{sr} Hz: need 16 kHz')
    return x if x.ndim == 1 else x.mean(1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--host', default='0.0.0.0')
    ap.add_argument('--port', type=int, default=8000)
    ap.add_argument('--threads', type=int, default=4)
    ap.add_argument('--models', default='/models')
    a = ap.parse_args()
    asr = Giga(a.models, a.threads)
    asr(np.zeros(SR, np.float32))  # the first call is slow: not the user's
    one = threading.Lock()  # the model takes one request at a time

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
                return self.send(200, {'engine': f'gigaam {MODEL}', 'device': 'cpu'})
            self.send(404, {})

        def do_POST(self):
            if self.path != '/v1/audio/transcriptions':
                return self.send(404, {})
            try:
                body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
                ctype = self.headers.get('Content-Type', '')
                x = audio(multipart(body, ctype)['file'] if 'multipart' in ctype else body)
            except (ValueError, KeyError, IndexError, RuntimeError) as e:
                return self.send(400, {'error': {'message': str(e)}})
            with one:
                text, seconds = asr(x)
            # how long it took, never what was said: the log is not a transcript
            print(time.strftime('%H:%M:%S'), f'{seconds:4.2f} s for {len(x) / SR:4.1f} s of audio', flush=True)
            self.send(200, {'text': text, 'seconds': round(seconds, 3)})

    print(time.strftime('%H:%M:%S'), f'speech recognition on {a.host}:{a.port} (gigaam {MODEL})', flush=True)
    ThreadingHTTPServer((a.host, a.port), Handler).serve_forever()


if __name__ == '__main__':
    main()
