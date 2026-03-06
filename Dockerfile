FROM rust:1-slim AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY tests/ tests/

RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /build/target/release/truss /truss

EXPOSE 8080

ENTRYPOINT ["/truss", "serve"]
