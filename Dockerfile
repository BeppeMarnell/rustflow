FROM rust:1.97.1-slim-bookworm AS builder

WORKDIR /app
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential cmake clang pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
ARG CARGO_BUILD_JOBS=2
RUN CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS cargo build --locked --release --bins

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 chatflow \
    && useradd --system --uid 10001 --gid chatflow --no-create-home chatflow
COPY --from=builder /app/target/release/api /usr/local/bin/api
COPY --from=builder /app/target/release/worker /usr/local/bin/worker
COPY --from=builder /app/target/release/bootstrap /usr/local/bin/bootstrap
USER chatflow
EXPOSE 3000
CMD ["api"]
