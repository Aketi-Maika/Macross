# Macross.yaml から各コンテナの設定を生成する（起動時に一度だけ動く）
FROM python:3.12-alpine
RUN pip install --no-cache-dir pyyaml==6.0.2
COPY render.py /usr/local/bin/render.py
CMD ["python3", "/usr/local/bin/render.py", "/etc/macross/Macross.yaml", "/generated"]
