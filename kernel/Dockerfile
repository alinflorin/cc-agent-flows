# The toolchain that builds the agent kernel (see build.sh). Native per architecture.
FROM debian:12-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential bc bison flex libelf-dev libssl-dev lz4 xz-utils curl ca-certificates cpio kmod \
    && rm -rf /var/lib/apt/lists/*
