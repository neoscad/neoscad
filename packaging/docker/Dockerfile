# ghcr.io/neoscad/neoscad: the `neoscad` CLI on Debian slim, with Mesa's
# software renderers so PNG export works in a container without a GPU.
#
# Built from a release archive, not from source, so the image holds the
# same binary as the release (glibc 2.28 build; Debian 12 has 2.36):
#
#   docker build -f packaging/docker/Dockerfile --build-arg NEOSCAD_VERSION=0.1.0 \
#     -t ghcr.io/neoscad/neoscad:0.1.0 packaging/docker
#
# For a local binary instead, pass --build-arg NEOSCAD_ARCHIVE=local and use
# as the context a directory holding `neoscad`, LICENSE, NOTICE and
# licenses/ (scripts/release/linux-packages.sh leaves one at
# <docker dir>/<arch>/stage).
#
#   docker run --rm -v "$PWD:/work" ghcr.io/neoscad/neoscad model.scad -o model.png
FROM debian:bookworm-slim AS fetch
ARG NEOSCAD_VERSION
ARG NEOSCAD_ARCHIVE=release
ARG TARGETARCH
COPY . /context
RUN set -eu; \
    case "$TARGETARCH" in \
        amd64) triple=x86_64-unknown-linux-gnu ;; \
        arm64) triple=aarch64-unknown-linux-gnu ;; \
        *) echo "unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    mkdir -p /out; \
    if [ "$NEOSCAD_ARCHIVE" = local ]; then \
        cp -r /context/neoscad /context/LICENSE /context/NOTICE /context/licenses /out/; \
    else \
        apt-get update; \
        apt-get install -y --no-install-recommends ca-certificates curl xz-utils; \
        base="https://github.com/neoscad/neoscad/releases/download/v${NEOSCAD_VERSION}"; \
        name="neoscad-cli-${triple}.tar.xz"; \
        curl -fsSLO "$base/$name"; \
        curl -fsSLO "$base/$name.sha256"; \
        sha256sum -c "$name.sha256"; \
        tar -xJf "$name"; \
        cp "neoscad-cli-${triple}/neoscad" /out/neoscad; \
        cp -r "neoscad-cli-${triple}/LICENSE" "neoscad-cli-${triple}/NOTICE" \
              "neoscad-cli-${triple}/licenses" /out/; \
    fi

FROM debian:bookworm-slim
# lavapipe (Vulkan) is what PNG export tries first; llvmpipe through EGL
# is its OpenGL fallback.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        mesa-vulkan-drivers libvulkan1 libegl1 libgles2 libgl1-mesa-dri \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -m 700 /tmp/xdg-runtime
# Mesa's Vulkan device-select layer asks Wayland for the default GPU, and
# without XDG_RUNTIME_DIR libwayland prints an error on every PNG export.
ENV XDG_RUNTIME_DIR=/tmp/xdg-runtime
COPY --from=fetch /out/ /usr/share/doc/neoscad/
RUN mv /usr/share/doc/neoscad/neoscad /usr/bin/neoscad && neoscad --version
LABEL org.opencontainers.image.source="https://github.com/neoscad/neoscad" \
      org.opencontainers.image.url="https://neoscad.org" \
      org.opencontainers.image.licenses="GPL-2.0-or-later" \
      org.opencontainers.image.description="OpenSCAD-compatible programmable solid CAD, with software rendering for PNG export"
WORKDIR /work
ENTRYPOINT ["neoscad"]
