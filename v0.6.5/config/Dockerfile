# Macross.yaml から各コンテナの設定を生成する（起動時に一度だけ動く）
# ビルドの起点は v0.6.5/（docker-compose.yml の build.context）。monitoring/ も入れておき、
# Kubernetes で動かすときは render.py が /generated/monitoring に写す
FROM python:3.12-alpine
RUN pip install --no-cache-dir pyyaml==6.0.2
COPY config/render.py /usr/local/bin/render.py
COPY monitoring/ /opt/macross/monitoring/
CMD ["python3", "/usr/local/bin/render.py", "/etc/macross/Macross.yaml", "/generated"]
