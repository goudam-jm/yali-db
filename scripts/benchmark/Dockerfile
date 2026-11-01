FROM rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6
RUN apt-get update && apt-get install -y --no-install-recommends python3 util-linux && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY yalidb-core ./yalidb-core
COPY yalidb-server ./yalidb-server
RUN cargo build -p yalidb-core --release --example benchmark --locked
COPY scripts/benchmark ./scripts/benchmark
RUN mkdir /work /results && chmod 0777 /work /results
ENTRYPOINT ["python3", "/app/scripts/benchmark/run.py"]
