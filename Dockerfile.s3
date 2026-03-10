FROM rust:1-slim-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release --locked --features s3

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -r truss && useradd -r -g truss -s /usr/sbin/nologin truss

COPY --from=builder /build/target/release/truss /truss

USER truss
EXPOSE 8080

ENTRYPOINT ["/truss", "serve"]
