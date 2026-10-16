# The Linux build environment, for `mise run linux:container`: an old glibc
# (Ubuntu 22.04's 2.35), so the AppImage runs on the distributions since,
# SteamOS included; Clang for the game's generated C; the headers SDL builds
# its Linux backends against (X11, Wayland, PipeWire/PulseAudio/ALSA, udev,
# D-Bus), whose libraries it loads at run time from the system: none of them
# is in the AppImage; ImageMagick for the icon; mise for the rest.
FROM ubuntu:22.04

ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential clang git curl ca-certificates pkg-config file imagemagick \
        libasound2-dev libpulse-dev libpipewire-0.3-dev \
        libx11-dev libxext-dev libxrandr-dev libxcursor-dev libxfixes-dev libxi-dev libxss-dev libxtst-dev \
        libxkbcommon-dev libwayland-dev libdecor-0-dev libegl-dev libgles-dev libdrm-dev libgbm-dev \
        libdbus-1-dev libudev-dev \
    && rm -rf /var/lib/apt/lists/*

ENV MISE_VERSION=v2026.7.5
ENV PATH=/root/.local/bin:$PATH
RUN curl -fsSL https://mise.run | sh

# The repository is mounted, owned by another user: git still works in it.
RUN git config --global --add safe.directory '*'
WORKDIR /glue5
