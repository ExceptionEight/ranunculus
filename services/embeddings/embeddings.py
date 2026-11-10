"""Embeddings for Ranunculus's memory (search by meaning): multilingual-e5-large-instruct on the CPU, about 3 GB of RAM.

    python embeddings.py [--host 0.0.0.0] [--port 8000] [--threads 8]

POST /embed   {"texts": [...], "kind": "query" | "doc"} -> {"vectors": [[...1024 floats...]], "dim": 1024}
GET  /health  -> {"model": ..., "dim": ...}
Questions get e5's instruction prefix, stored exchanges none. Vectors are L2-normalized (dot = cosine). fp32: int8
lost accuracy on Russian questions asked in other words. The model (about 2 GB) is downloaded from Hugging Face at
the first start, into HF_HOME."""

import argparse
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODEL = 'intfloat/multilingual-e5-large-instruct'
QUERY = 'Instruct: Given a question, retrieve the past conversation that answers it\nQuery: '


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--host', default='0.0.0.0')
    ap.add_argument('--port', type=int, default=8000)
    ap.add_argument('--threads', type=int, default=8)
    a = ap.parse_args()
    import torch
    from sentence_transformers import SentenceTransformer

    torch.set_num_threads(a.threads)
    model = SentenceTransformer(MODEL, device='cpu', model_kwargs={'dtype': torch.float32})
    dim = model.get_sentence_embedding_dimension()
    one = threading.Lock()
    model.encode(['проверка'], normalize_embeddings=True)  # the first call is slow: not the user's

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def send(self, code, obj):
            body = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path == '/health':
                return self.send(200, {'model': MODEL, 'dim': dim})
            self.send(404, {})

        def do_POST(self):
            if self.path != '/embed':
                return self.send(404, {})
            try:
                req = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
                texts = [str(t) for t in req['texts']][:256]
                prefix = QUERY if req.get('kind') == 'query' else ''
            except (ValueError, KeyError, TypeError):
                return self.send(400, {'error': 'need {"texts": [...], "kind": "query"|"doc"}'})
            with one:
                vectors = model.encode([prefix + t for t in texts], normalize_embeddings=True, batch_size=16)
            self.send(200, {'vectors': [[round(float(x), 6) for x in row] for row in vectors], 'dim': dim})

    print(f'embeddings on {a.host}:{a.port} ({MODEL}, dim {dim})', flush=True)
    ThreadingHTTPServer((a.host, a.port), Handler).serve_forever()


if __name__ == '__main__':
    main()
