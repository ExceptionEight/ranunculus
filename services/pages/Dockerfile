# Web pages as text (the read_page tool): the standard library only.   docker build -t ranunculus-pages services/pages
FROM python:3.14-slim
WORKDIR /app
COPY pages.py webpage.py ./
USER 1000:1000
EXPOSE 8000
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8000/health', timeout=4)"
CMD ["python", "pages.py"]
