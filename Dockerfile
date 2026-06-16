FROM node:24-bookworm-slim AS frontend

RUN npm install -g pnpm@10
WORKDIR /app/web
COPY web/package.json web/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm run build

FROM rust:1.94-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends cmake libopus-dev libssl-dev pkg-config && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY vendor/ vendor/
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src

COPY src/ src/
COPY migrations/ migrations/
RUN touch src/main.rs && cargo build --release

FROM node:24-bookworm-slim AS runtime-base

# curl is required at runtime by CommandWhisperClient for whisper.cpp inference.
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates libopus0 libssl3 && rm -rf /var/lib/apt/lists/*

RUN groupadd -r app && useradd -r -g app -m -d /home/app app
RUN mkdir -p /data/chunks && chown app:app /data/chunks

COPY --from=builder /app/target/release/discord-transcript /usr/local/bin/discord-transcript
COPY --from=frontend /app/web/dist /app/web/dist

USER app
WORKDIR /app
ENV HOME=/home/app
ENV STATIC_FILES_DIR=/app/web/dist
EXPOSE 3000

FROM runtime-base AS unsafe-claude

# Unsafe local harness target only. Production builds use the final production stage.
ARG CLAUDE_CODE_VERSION=2.1.178
ARG CLAUDE_CODE_INTEGRITY=sha512-XhNQu2QkthrwznlzQA1ZrnRmApA2fgjQ2jxlcPP5B581Tg/E5+hUcbWTkW4OkPDgXg9KljtK7XZ20KszNxt8xg==
ARG CLAUDE_CODE_LINUX_X64_INTEGRITY=sha512-W3XUZDHi3XtsftWK+phsnPyKWx5y7ULfyCiNQFLH5LNih73v0/wRg5t/Kqtj8+rl4pI0LCgcJW2USeK/7CCmvQ==
ARG CLAUDE_CODE_LINUX_ARM64_INTEGRITY=sha512-tDdcLyUNahnj71lj5c/BEsAHt6Ad1KjRznfzE2IuHflUrHgQtCpObjV8QK2YtoDVgDaPT7frGekse/SK6eKweQ==
ARG TARGETARCH
USER root
RUN set -eux; \
    arch="${TARGETARCH:-$(uname -m)}"; \
    case "$arch" in \
      amd64|x86_64) platform_package="@anthropic-ai/claude-code-linux-x64"; platform_integrity="$CLAUDE_CODE_LINUX_X64_INTEGRITY" ;; \
      arm64|aarch64) platform_package="@anthropic-ai/claude-code-linux-arm64"; platform_integrity="$CLAUDE_CODE_LINUX_ARM64_INTEGRITY" ;; \
      *) echo "unsupported Claude Code TARGETARCH: $arch" >&2; exit 1 ;; \
    esac; \
    verify_integrity='const [name, version, expected] = process.argv.slice(1); const encoded = encodeURIComponent(name); const res = await fetch(`https://registry.npmjs.org/${encoded}/${version}`); if (!res.ok) { throw new Error(`metadata fetch failed for ${name}@${version}: ${res.status}`); } const meta = await res.json(); const actual = meta?.dist?.integrity; if (actual !== expected) { throw new Error(`${name}@${version} integrity mismatch: expected ${expected}, got ${actual}`); }'; \
    node --input-type=module -e "$verify_integrity" @anthropic-ai/claude-code "$CLAUDE_CODE_VERSION" "$CLAUDE_CODE_INTEGRITY"; \
    node --input-type=module -e "$verify_integrity" "$platform_package" "$CLAUDE_CODE_VERSION" "$platform_integrity"; \
    npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"; \
    claude_version="$(claude --version)"; \
    case "$claude_version" in \
      *"$CLAUDE_CODE_VERSION"*) printf '%s\n' "$claude_version" ;; \
      *) echo "unexpected Claude Code version: $claude_version" >&2; exit 1 ;; \
    esac
USER app
WORKDIR /app
CMD ["discord-transcript"]

FROM runtime-base AS production

USER app
WORKDIR /app
CMD ["discord-transcript"]
