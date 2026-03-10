FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# ── 1. Cache dependencies ────────────────────────────────────────────
# Copy only the manifest files and build a dummy library/binary so that
# dependency crates are compiled and cached in their own layer.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo '' > src/lib.rs \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked --features "s3,gcs,azure" \
    && rm -rf src

# ── 2. Build the real binary ─────────────────────────────────────────
COPY src/ src/
RUN touch src/lib.rs src/main.rs \
    && cargo build --release --locked --features "s3,gcs,azure"

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -r truss && useradd -r -g truss -s /usr/sbin/nologin truss

COPY --from=builder /build/target/release/truss /truss

USER truss
EXPOSE 8080

ENTRYPOINT ["/truss", "serve"]
