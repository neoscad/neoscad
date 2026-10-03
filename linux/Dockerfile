# A build environment for the Linux desktop app (crates/linux-app), for
# working on it from macOS or from a distribution without GTK 4.14,
# libadwaita 1.5 and WebKitGTK 6.0. Ubuntu 24.04 has all three, which is
# also why CI's app job runs on ubuntu-24.04 (22.04 has GTK 4.6 and no
# webkitgtk-6.0). Mesa's lavapipe (Vulkan) and llvmpipe (GL) stand in for
# a GPU and Xvfb for a display, so the app runs headless here.
# librsvg2-common is gdk-pixbuf's SVG loader, a recommend that
# --no-install-recommends leaves out: without it Adwaita's symbolic icons
# draw as "missing image" in the smoke test's screenshots.
# docs/linux-app.md has the commands.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl build-essential pkg-config git \
        libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev \
        mesa-vulkan-drivers libvulkan1 libegl1 libgles2 libgl1-mesa-dri \
        xvfb xauth dbus-x11 imagemagick xdotool fonts-cantarell adwaita-icon-theme \
        librsvg2-common \
    && rm -rf /var/lib/apt/lists/*

# rustup with the toolchain rust-toolchain.toml pins, so the first build
# does not download it again.
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain 1.98.1 -c clippy -c rustfmt \
    && chmod -R a+w "$RUSTUP_HOME" "$CARGO_HOME"

# Mesa's Vulkan device-select layer asks Wayland for the default GPU, and
# without XDG_RUNTIME_DIR libwayland prints an error on every GPU open
# (as in packaging/docker/build.Dockerfile).
ENV XDG_RUNTIME_DIR=/tmp/xdg
RUN mkdir -p /tmp/xdg && chmod 700 /tmp/xdg
WORKDIR /src
