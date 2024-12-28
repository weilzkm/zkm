FROM rustlang/rust:nightly AS builder
RUN apt-get update && apt-get install -y \
    autoconf \
    automake \
    libtool \
    curl \
    make \
    gcc \
    g++ \
    unzip \
    pkg-config \
    openssl \
    libssl-dev \
    wget \
    vim \
    git \
    cmake \
    ninja-build \
    openssl-devel \
    zstd \
    xz \
    && rm -rf /var/lib/apt/lists/*

# install mips target
RUN \
  curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/zkMIPS/toolchain/refs/heads/main/setup.sh | sh \

ENV PATH=$HOME/.zkm-toolchain/rust-toolchain-x86-64-unknown-linux-gnu-20241217/bin:$PATH

# install golang
ENV GOLANG_VERSION=1.23.2
ENV GOLANG_DOWNLOAD_URL=https://go.dev/dl/
ENV GOLANG_DOWNLOAD_SHA256_AMD64=542d3c1705f1c6a1c5a80d5dc62e2e45171af291e755d591c5e6531ef63b454e
ENV GOLANG_DOWNLOAD_SHA256_ARM64=f626cdd92fc21a88b31c1251f419c17782933a42903db87a174ce74eeecc66a9

RUN ARCH=$(uname -m) && \
    if [ "$ARCH" = "x86_64" ]; then \
        GOARCH=amd64; \
        GO_SHA256=$GOLANG_DOWNLOAD_SHA256_AMD64; \
    elif [ "$ARCH" = "aarch64" ]; then \
        GOARCH=arm64; \
        GO_SHA256=$GOLANG_DOWNLOAD_SHA256_ARM64; \
    else \
        echo "Unsupported architecture"; exit 1; \
    fi && \
    wget ${GOLANG_DOWNLOAD_URL}go${GOLANG_VERSION}.linux-${GOARCH}.tar.gz && \
    echo "${GO_SHA256} go${GOLANG_VERSION}.linux-${GOARCH}.tar.gz" | sha256sum -c - && \
    tar -C /usr/local -xzf go${GOLANG_VERSION}.linux-${GOARCH}.tar.gz && \
    rm go${GOLANG_VERSION}.linux-${GOARCH}.tar.gz

ENV PATH=/usr/local/go/bin:$PATH

# docker build -t zkm/zkmips:compile .
# docker run -it --rm -v ./:/zkm zkm/zkmips:compile
# compile rust mips
# cd /zkm/prover/examples/sha2-rust && cargo build -r --target=mips-unknown-linux-musl
# cd /zkm/prover/examples/revme && cargo build -r --target=mips-unknown-linux-musl
# compile go mips
# cd /zkm/prover/examples/add-go && GOOS=linux GOARCH=mips GOMIPS=softfloat go build .
# cd /zkm/prover/examples/sha2-go && GOOS=linux GOARCH=mips GOMIPS=softfloat go build .