FROM rust:1.87-slim AS builder

# Install dependencies
RUN apt-get update && apt-get install -y \
    curl unzip build-essential git pkg-config

# Install Zig
ENV ZIG_VERSION=0.12.0
RUN curl -LO https://ziglang.org/download/${ZIG_VERSION}/zig-linux-x86_64-${ZIG_VERSION}.tar.xz \
    && tar -xf zig-linux-x86_64-${ZIG_VERSION}.tar.xz \
    && mv zig-linux-x86_64-${ZIG_VERSION} /opt/zig
ENV PATH="/opt/zig:$PATH"

# Install cargo-zigbuild
RUN cargo install cargo-zigbuild

# Add the target
RUN rustup target add aarch64-unknown-linux-musl

# Create app dir
WORKDIR /app

# Copy only Cargo.toml and Cargo.lock to leverage Docker layer caching
COPY Cargo.toml Cargo.lock ./
RUN sed -i '/^version *= *".*"/d' Cargo.toml

# Pre-create a dummy src/lib.rs so build doesn't fail
RUN mkdir src && echo "fn main() {}" > src/main.rs

# Build dependencies only — this will be cached unless Cargo.toml/lock changes
RUN cargo zigbuild --release --target aarch64-unknown-linux-musl || true

# Now copy the actual source
COPY . .

# Final build
RUN cargo zigbuild --release --target aarch64-unknown-linux-musl

# Final image
FROM scratch
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /app/target/aarch64-unknown-linux-musl/release/ops /ops
ENTRYPOINT ["/ops"]
CMD ["env"]
