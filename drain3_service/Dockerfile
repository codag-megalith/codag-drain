# Stage 1: Build
FROM rust:1.86-slim AS builder
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release -p drain3_service

# Stage 2: Runtime
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/drain3_service /usr/local/bin/drain3_service
ENV PERSISTENCE_BACKEND=file
ENV PERSISTENCE_FILE_PATH=/data/drain3_state.json
EXPOSE 3000
CMD ["drain3_service"]
