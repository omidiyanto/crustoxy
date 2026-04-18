FROM rust:1.95-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src/ src/
RUN cargo build --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg && \
    curl -fsSL https://pkg.cloudflareclient.com/pubkey.gpg | gpg --dearmor -o /usr/share/keyrings/cloudflare-warp-archive-keyring.gpg && \
    echo "deb [signed-by=/usr/share/keyrings/cloudflare-warp-archive-keyring.gpg] https://pkg.cloudflareclient.com/ bookworm main" > /etc/apt/sources.list.d/cloudflare-client.list && \
    apt-get update && \
    apt-get install -y --no-install-recommends cloudflare-warp && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/crustoxy /usr/local/bin/crustoxy
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
ENV HOST=0.0.0.0
ENV PORT=8082
EXPOSE 8082
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]