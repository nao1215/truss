FROM rust:1-slim-bookworm AS planner
RUN cargo install cargo-chef --locked
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY benches/ benches/
RUN cargo chef prepare --recipe-path recipe.json

FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-chef --locked
WORKDIR /build
COPY --from=planner /build/recipe.json .
COPY benches/ benches/
RUN cargo chef cook --release --features "s3,gcs,azure" --recipe-path recipe.json

COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY benches/ benches/

RUN cargo build --release --locked --features "s3,gcs,azure"

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -r truss && useradd -r -g truss -s /usr/sbin/nologin truss

COPY --from=builder /build/target/release/truss /truss

USER truss
EXPOSE 8080

ENTRYPOINT ["/truss", "serve"]
