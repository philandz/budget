FROM rust:1.88-bookworm AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler \
    libprotobuf-dev \
    pkg-config \
    libssl-dev \
  && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY src ./src
COPY migrations ./migrations
COPY protobuf ./protobuf
COPY libs ./libs

RUN ln -sfn /app/libs /libs && ln -sfn /app/protobuf /protobuf

RUN cargo build --release

FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/budget /usr/local/bin/budget
COPY --from=builder /app/migrations /app/migrations

EXPOSE 9103 50103
ENTRYPOINT ["/usr/local/bin/budget"]
