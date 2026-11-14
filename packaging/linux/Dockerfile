# The Linux release image (docs/DECISIONS.md D-066): build tools only, no source and no build output.
# Ubuntu 20.04 on arm64, so the packages need glibc 2.31 or newer. arm64 builds natively; x86_64 is
# cross-compiled here (gcc-x86-64-linux-gnu, the amd64 libraries through multiarch), never emulated.
# scripts/bundle-linux.sh builds it and runs it with the source mounted read-only and the cargo
# registry and target/ in the volume kerno-linux-cache:
#
#   docker build --platform linux/arm64 -t kerno-linux-build \
#     --build-arg RUST_TOOLCHAIN=<rust-toolchain.toml channel> packaging/linux
#
# Every download is pinned: the base image by digest, rustup-init and the AppImage runtime by version
# and SHA-256. apt packages come from Ubuntu's archive (signed).
FROM ubuntu:20.04@sha256:8feb4d8ca5354def3d8fce243717141ce31e2c428701f6682bd2fafe15388214

ARG RUST_TOOLCHAIN
ENV DEBIAN_FRONTEND=noninteractive

# arm64 comes from ports.ubuntu.com, amd64 from archive.ubuntu.com and security.ubuntu.com.
RUN sed -i 's/^deb http/deb [arch=arm64] http/' /etc/apt/sources.list \
 && for suite in focal focal-updates; do \
      echo "deb [arch=amd64] http://archive.ubuntu.com/ubuntu $suite main universe" >> /etc/apt/sources.list; \
    done \
 && echo "deb [arch=amd64] http://security.ubuntu.com/ubuntu focal-security main universe" >> /etc/apt/sources.list \
 && dpkg --add-architecture amd64 \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl pkg-config make gcc g++ clang dpkg-dev file binutils squashfs-tools \
      crossbuild-essential-amd64 binutils-x86-64-linux-gnu \
      libfontconfig1-dev libxkbcommon-x11-dev libx11-xcb-dev libwayland-dev \
      libfontconfig1-dev:amd64 libxkbcommon-x11-dev:amd64 libx11-xcb-dev:amd64 libwayland-dev:amd64 \
 && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf -o /tmp/rustup-init \
      https://static.rust-lang.org/rustup/archive/1.29.3/aarch64-unknown-linux-gnu/rustup-init \
 && echo "9f608c142e0e38a78957d0661ba1dfaf0b6fb37184cfaa4b9a0aa57af6184282  /tmp/rustup-init" | sha256sum -c - \
 && chmod +x /tmp/rustup-init \
 && /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain none \
 && rm /tmp/rustup-init \
 && rustup toolchain install "$RUST_TOOLCHAIN" --profile minimal --component rustfmt,clippy \
      --target aarch64-unknown-linux-gnu,x86_64-unknown-linux-gnu

# The AppImage type-2 runtime (github.com/AppImage/type2-runtime, release 20251108), one per arch.
RUN mkdir -p /opt/appimage \
 && curl --proto '=https' --tlsv1.2 -sSfL -o /opt/appimage/runtime-aarch64 \
      https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-aarch64 \
 && curl --proto '=https' --tlsv1.2 -sSfL -o /opt/appimage/runtime-x86_64 \
      https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64 \
 && cd /opt/appimage \
 && printf '%s\n' \
      "00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444  runtime-aarch64" \
      "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d  runtime-x86_64" \
    | sha256sum -c -

# Cross-compilation to x86_64: the linker, the C compilers of build scripts, and pkg-config pointed at
# the amd64 libraries (target-suffixed, so host build scripts keep the arm64 ones).
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++ \
    AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar \
    PKG_CONFIG_ALLOW_CROSS_x86_64_unknown_linux_gnu=1 \
    PKG_CONFIG_LIBDIR_x86_64_unknown_linux_gnu=/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    KERNO_LINUX_BUILD_IMAGE=1

WORKDIR /src
