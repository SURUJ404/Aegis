# Multi-stage build for any lq binary.
#   docker build -f docker/Dockerfile -t lq/trading-engine:0.1.0 .
#   docker build --build-arg BIN=market-data-service -t lq/market-data-service .
# BIN is the PACKAGE name (trading-engine | market-data-service | backtest-runner |
# simulator | api-server). The runtime image contains that binary as /app.

FROM rust:1.88-slim AS builder
ARG BIN=trading-engine
WORKDIR /src
COPY . .
RUN cargo build --release -p $BIN \
    && case "$BIN" in \
         simulator) EXE=simulate ;; \
         backtest-runner) EXE=backtest ;; \
         *) EXE=$BIN ;; \
       esac \
    && mkdir -p /out \
    && cp target/release/$EXE /out/app

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /out/app /usr/local/bin/app
COPY docker/config/lq.toml /config/lq.toml
ENV LQ_CONFIG=/config/lq.toml
EXPOSE 8080 9100
CMD ["/usr/local/bin/app"]