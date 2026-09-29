# The Linux build and test environment of scripts/release/linux-docker.sh:
# the pinned toolchain plus Mesa's software renderers, so `cargo test`,
# `conformance run` and a PNG export all work in a container with no GPU.
#
# lavapipe (mesa-vulkan-drivers) is the Vulkan path `neoscad -o x.png`
# tries first; llvmpipe through EGL (libegl1, libgles2, libgl1-mesa-dri) is
# the OpenGL fallback. git lets the conformance harness name the reference
# commit, and poppler-utils rasterises the PDF export cases (as on the Mac;
# without it those six cases fail with "no PDF rasteriser").
FROM rust:1.98.1-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        mesa-vulkan-drivers libvulkan1 \
        libegl1 libgles2 libgl1-mesa-dri \
        git poppler-utils \
    && rm -rf /var/lib/apt/lists/*

# What rust-toolchain.toml asks for, installed once here rather than by
# rustup in every (disposable) container.
RUN rustup component add rustfmt clippy \
    && rustup target add wasm32-unknown-unknown

# Mesa's Vulkan device-select layer asks Wayland for the default GPU, and
# libwayland prints "error: XDG_RUNTIME_DIR is invalid or not set" on every
# device open when the variable is missing, as it is in a container. The
# line lands in neoscad's stderr and broke the serve test that compares a
# served PNG export's stderr with a direct one.
ENV XDG_RUNTIME_DIR=/tmp/xdg-runtime
RUN mkdir -m 700 /tmp/xdg-runtime
