FROM node:22-bookworm-slim AS frontend

RUN npm install -g pnpm@10
WORKDIR /app/web
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm run build

FROM rust:1.94-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends cmake libopus-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src

COPY src/ src/
RUN touch src/main.rs && cargo build --release

FROM node:22-bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates libopus0 && rm -rf /var/lib/apt/lists/*
RUN npm install -g @anthropic-ai/claude-code

RUN groupadd -r app && useradd -r -g app -m -d /home/app app
RUN mkdir -p /data/chunks && chown app:app /data/chunks

COPY --from=builder /app/target/release/discord-transcript /usr/local/bin/discord-transcript
COPY --from=frontend /app/web/dist /app/web/dist

USER app
WORKDIR /app
ENV HOME=/home/app
ENV STATIC_FILES_DIR=/app/web/dist
EXPOSE 3000

CMD ["discord-transcript"]
