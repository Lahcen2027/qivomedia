# ---- build ----
FROM rust:1-bookworm AS build
WORKDIR /src
# Cache dependencies separately from our code.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src
COPY migrations ./migrations
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tzdata \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data qivo \
 && mkdir -p /data && chown qivo:qivo /data
WORKDIR /app
COPY --from=build /src/target/release/qivo-server /usr/local/bin/qivo-server
COPY web ./web
ENV QIVO_BIND=0.0.0.0:8080 QIVO_DATA_DIR=/data QIVO_WEB_DIR=/app/web
USER qivo
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s CMD ["qivo-server", "healthcheck"]
CMD ["qivo-server"]
