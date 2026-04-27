# syntax=docker/dockerfile:1

ARG RUST_VERSION=1
ARG OBSCURA_VERSION=v0.1.1

FROM rust:${RUST_VERSION}-bookworm AS gateway-builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release --locked

FROM ubuntu:24.04 AS obscura-downloader
ARG OBSCURA_VERSION
ARG TARGETARCH=amd64
ARG OBSCURA_ASSET

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tar \
    && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    if [ -z "${OBSCURA_ASSET:-}" ]; then \
      case "${TARGETARCH}" in \
        amd64) OBSCURA_ASSET="obscura-x86_64-linux.tar.gz" ;; \
        *) echo "No default Obscura release asset for TARGETARCH=${TARGETARCH}. Set OBSCURA_ASSET explicitly." >&2; exit 1 ;; \
      esac; \
    fi; \
    mkdir -p /obscura; \
    curl -fsSL "https://github.com/h4ckf0r0day/obscura/releases/download/${OBSCURA_VERSION}/${OBSCURA_ASSET}" \
      | tar -xz -C /obscura; \
    test -x /obscura/obscura

FROM ubuntu:24.04 AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates python3-minimal tini \
    && rm -rf /var/lib/apt/lists/*

COPY --from=gateway-builder /app/target/release/obscura-gateway /usr/local/bin/obscura-gateway
COPY --from=gateway-builder /app/target/release/obscura-cli /usr/local/bin/obscura-cli
COPY --from=obscura-downloader /obscura/obscura /usr/local/bin/obscura
COPY docker/entrypoint.sh /usr/local/bin/obscura-gateway-entrypoint

ENV HOME=/data \
    OBSCURA_GATEWAY_STATE_ROOT=/data/.obscura-gateway \
    OBSCURA_GATEWAY_LISTEN_ADDR=0.0.0.0:18789 \
    OBSCURA_GATEWAY_SERVER_URL=http://127.0.0.1:18789 \
    OBSCURA_GATEWAY_OBSCURA_BIN=/usr/local/bin/obscura

WORKDIR /data
VOLUME ["/data/.obscura-gateway"]
EXPOSE 18789

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/obscura-gateway-entrypoint"]
CMD ["run"]
