FROM rust:1.85-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src

COPY src/ src/
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash mediaforge

COPY --from=builder /build/target/release/mediaforge /usr/local/bin/mediaforge

USER mediaforge
WORKDIR /home/mediaforge

VOLUME ["/media", "/config", "/cache"]

ENV RUST_LOG=info

EXPOSE 8484

ENTRYPOINT ["mediaforge"]
CMD ["serve", "-c", "/config/config.toml"]
