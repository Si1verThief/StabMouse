FROM mcr.microsoft.com/devcontainers/rust:1-bookworm

# Build dependencies:
#   - Slint (winit + femtovg): fontconfig, freetype, xkbcommon, wayland, GL/EGL, X11 fallback
#   - evdev/udev crates: libudev
#   - device inspection while developing: evtest, usbutils
#   - offline filter-research harness: python3 + numpy + matplotlib
RUN apt-get update && export DEBIAN_FRONTEND=noninteractive \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        libfontconfig1-dev \
        libfreetype6-dev \
        libxkbcommon-dev \
        libwayland-dev \
        wayland-protocols \
        libgl1-mesa-dev \
        libegl1-mesa-dev \
        libx11-dev \
        libxcursor-dev \
        libxrandr-dev \
        libxi-dev \
        libudev-dev \
        evtest \
        usbutils \
        python3 \
        python3-numpy \
        python3-matplotlib \
    && apt-get clean && rm -rf /var/lib/apt/lists/*
