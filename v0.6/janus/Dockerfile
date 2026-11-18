# Janus Gateway (AudioBridge = MCU)
# 設定は config サービスが Macross.yaml から /generated/janus に生成する
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends janus ca-certificates \
 && rm -rf /var/lib/apt/lists/*

CMD ["janus", "-F", "/generated/janus"]
