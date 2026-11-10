"""Silero VAD (the silero_vad package's JIT model, its 16 kHz part) -> safetensors with the names crates/vad reads:
how crates/vad/assets/silero_vad_16k.safetensors was made. The package's own silero_vad_16k.safetensors holds other
weights (a different checkpoint: every conv differs), so it is not used.

    uv run --with silero-vad==6.2.3 --with numpy tools/export_vad.py crates/vad/assets/silero_vad_16k.safetensors"""

import json
import os
import struct
import sys

import numpy as np
import silero_vad
import torch

d = os.path.join(os.path.dirname(silero_vad.__file__), 'data')
m = torch.jit.load(os.path.join(d, 'silero_vad.jit'), map_location='cpu')._model
sd = {k: v.numpy().astype('<f4') for k, v in m.state_dict().items()}
names = {
    'stft_conv.weight': 'stft.forward_basis_buffer',
    'final_conv.weight': 'decoder.decoder.2.weight',
    'final_conv.bias': 'decoder.decoder.2.bias',
}
for i in range(4):
    for p in ('weight', 'bias'):
        names[f'conv{i + 1}.{p}'] = f'encoder.{i}.reparam_conv.{p}'
for p in ('weight_ih', 'weight_hh', 'bias_ih', 'bias_hh'):
    names[f'lstm_cell.{p}'] = f'decoder.rnn.{p}'
head, blobs, off = (
    {
        '__metadata__': {
            'source': f'silero_vad {getattr(silero_vad, "__version__", "?")} silero_vad.jit _model (16 kHz)'
        }
    },
    [],
    0,
)
for k, src in names.items():
    a = np.ascontiguousarray(sd[src])
    head[k] = {'dtype': 'F32', 'shape': list(a.shape), 'data_offsets': [off, off + a.nbytes]}
    blobs.append(a.tobytes())
    off += a.nbytes
h = json.dumps(head).encode()
h += b' ' * (-len(h) % 8)
with open(sys.argv[1], 'wb') as f:
    f.write(struct.pack('<Q', len(h)) + h + b''.join(blobs))
print(sys.argv[1], off, 'bytes of weights')
