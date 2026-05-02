FROM rust:1.95-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src ./target/release/deps/crustoxy*
COPY src/ src/
RUN touch src/main.rs && cargo build --release

# Download Windsurf language server binary (optional, used when WINDSURF_API_KEY is set)
FROM debian:bookworm-slim AS windsurf-dl
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /opt/windsurf && \
    ASSET="language_server_linux_x64" && \
    PRIMARY_API="https://api.github.com/repos/Exafunction/codeium/releases/latest" && \
    FALLBACK_URL="https://github.com/dwgx/WindsurfAPI/releases/latest/download/${ASSET}" && \
    echo "Fetching release info from Exafunction..." && \
    URL="$(curl -fsSL "${PRIMARY_API}" | grep -oE "https://[^\"]+/${ASSET}" | head -1)" || true && \
    if [ -n "$URL" ] && curl -fL --retry 3 -o "/opt/windsurf/${ASSET}" "${URL}"; then \
      echo "Downloaded successfully from Exafunction"; \
    else \
      echo "Exafunction failed, trying WindsurfAPI fallback..."; \
      if curl -fL --retry 3 -o "/opt/windsurf/${ASSET}" "${FALLBACK_URL}"; then \
        echo "Downloaded from WindsurfAPI fallback"; \
      else \
        echo "ERROR: Could not download LS binary from any source" && exit 1; \
      fi; \
    fi && \
    chmod +x "/opt/windsurf/${ASSET}"

FROM debian:bookworm-slim AS runtime
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg dbus && \
    curl -fsSL https://pkg.cloudflareclient.com/pubkey.gpg | gpg --dearmor -o /usr/share/keyrings/cloudflare-warp-archive-keyring.gpg && \
    echo "deb [signed-by=/usr/share/keyrings/cloudflare-warp-archive-keyring.gpg] https://pkg.cloudflareclient.com/ bookworm main" > /etc/apt/sources.list.d/cloudflare-client.list && \
    apt-get update && \
    apt-get install -y --no-install-recommends cloudflare-warp && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/crustoxy /usr/local/bin/crustoxy
COPY --from=windsurf-dl /opt/windsurf/ /opt/windsurf/
RUN mkdir -p /opt/windsurf/data/db
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
ENV HOST=0.0.0.0
ENV PORT=8082
EXPOSE 8082
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]