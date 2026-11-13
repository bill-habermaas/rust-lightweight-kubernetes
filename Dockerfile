# ---- build ----------------------------------------------------------------
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY proto proto
COPY src src
COPY examples examples
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin lkube-server --example webapp_operator \
 && mkdir -p /out \
 && cp target/release/lkube-server target/release/examples/webapp_operator /out/

# ---- runtime --------------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/lkube-server /usr/local/bin/lkube-server
COPY --from=build /out/webapp_operator /usr/local/bin/webapp-operator
ENV LKUBE_LISTEN=0.0.0.0:50051 RUST_LOG=info
EXPOSE 50051
# Runs as root so it can use the mounted Docker socket (the container runtime).
ENTRYPOINT ["/usr/local/bin/lkube-server"]
