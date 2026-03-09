FROM rust:1-slim AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      git curl ca-certificates file && \
    curl -fsSL https://git.io/shellspec | sh -s -- --yes && \
    apt-get purge -y curl git && apt-get autoremove -y && rm -rf /var/lib/apt/lists/*
ENV PATH="/root/.local/lib/shellspec:${PATH}"
COPY --from=builder /build/target/release/truss /usr/local/bin/truss
COPY integration/cli/ /src/
COPY integration/fixtures/ /fixtures/
WORKDIR /src
ENTRYPOINT ["shellspec"]
