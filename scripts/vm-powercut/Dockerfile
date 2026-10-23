FROM rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends python3 busybox-static cpio kmod e2fsprogs xfsprogs qemu-system-arm qemu-system-x86 linux-image-$(dpkg --print-architecture) && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY yalidb-core ./yalidb-core
COPY yalidb-server ./yalidb-server
RUN cargo build -p yalidb-core --release --example storage_probe --locked
COPY scripts/vm-powercut ./scripts/vm-powercut
COPY scripts/run-vm-powercut.py ./scripts/run-vm-powercut.py
RUN python3 scripts/vm-powercut/make-initrd.py && mkdir /work /results && chmod 0777 /work /results
ENTRYPOINT ["python3", "/app/scripts/vm-powercut/run.py"]
