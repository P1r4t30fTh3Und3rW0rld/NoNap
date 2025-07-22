# Stage 1: Build the Rust binary
FROM rust:1.72 as builder

WORKDIR /app

# Cache dependencies for faster rebuilds
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -rf src

# Copy source code and build actual app
COPY . .
RUN cargo build --release

# Stage 2: Create a lightweight runtime image
FROM debian:buster-slim

# Install dependencies for running the app
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

# Copy binary and config from builder
COPY --from=builder /app/target/release/nonap /usr/local/bin/nonap
COPY --from=builder /app/targets.json /app/targets.json

WORKDIR /app

EXPOSE 3030

CMD ["nonap"]
