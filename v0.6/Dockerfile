# Macross サーバ (Rust) + フロントエンド (Vue)

FROM node:22-bookworm-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-fund --no-audit
COPY web/ ./
RUN npm run build

FROM rust:1-bookworm AS server
WORKDIR /src
COPY server/ ./
RUN cargo build --release

FROM debian:bookworm-slim
# janus-pp-rec (.mjr → .opus) と ffmpeg (.opus → .mp3) を録音変換に、
# open-jtalk（辞書・音声モデル）をテキスト読み上げに使う（音声モデルは Debian の contrib にある）
RUN sed -i 's/^Components: main$/Components: main contrib/' /etc/apt/sources.list.d/debian.sources \
 && apt-get update \
 && apt-get install -y --no-install-recommends janus-tools ffmpeg ca-certificates \
      open-jtalk open-jtalk-mecab-naist-jdic hts-voice-nitech-jp-atr503-m001 \
 && rm -rf /var/lib/apt/lists/*
COPY --from=server /src/target/release/macross /usr/local/bin/macross
COPY --from=web /web/dist /opt/macross/web
ENV MACROSS_WEB_DIR=/opt/macross/web
CMD ["macross", "--config", "/etc/macross/Macross.yaml"]
