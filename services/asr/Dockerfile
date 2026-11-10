# Speech recognition: GigaAM on the CPU.   docker build -t ranunculus-asr services/asr
# 3.13, not 3.14: GigaAM pins onnxruntime 1.23, which has no wheels for 3.14
FROM python:3.13-slim
RUN pip install --no-cache-dir torch==2.15.0 torchaudio==2.11.1 --index-url https://download.pytorch.org/whl/cpu
COPY requirements.txt /tmp/
RUN pip install --no-cache-dir -r /tmp/requirements.txt && mkdir /models && chown 1000:1000 /models
WORKDIR /app
COPY asr.py ./
USER 1000:1000
VOLUME /models
EXPOSE 8000
HEALTHCHECK --interval=30s --timeout=5s --start-period=10m CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8000/health', timeout=4)"
CMD ["python", "asr.py"]
