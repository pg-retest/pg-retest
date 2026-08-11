# Stage 1: Build
FROM rust:1.90-bookworm AS builder
WORKDIR /src
# pg_query's build.rs uses bindgen, which needs clang + libclang headers.
RUN apt-get update && apt-get install -y --no-install-recommends libclang-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/pg-retest /usr/local/bin/pg-retest
RUN mkdir -p /data/workloads
EXPOSE 8080
ENTRYPOINT ["pg-retest"]
CMD ["web", "--port", "8080", "--data-dir", "/data", "--bind", "0.0.0.0", "--no-auth"]
