# Janus Gateway (AudioBridge = MCU)
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends janus python3 python3-yaml ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY render_config.py /usr/local/bin/render_config.py

# Macross.yaml から設定を生成してから起動
CMD ["sh", "-c", "python3 /usr/local/bin/render_config.py /etc/macross/Macross.yaml /run/janus-config && exec janus -F /run/janus-config"]
