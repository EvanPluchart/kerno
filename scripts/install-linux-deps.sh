#!/usr/bin/env bash
# System packages needed to build and test Kerno on Debian/Ubuntu (GPUI's
# native dependencies, a headless X server for the UI tests, and the client
# tools Kerno calls for backup and restore). Run as root.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends \
  ca-certificates curl git pkg-config make cmake gcc g++ clang \
  libfontconfig-dev libwayland-dev libxkbcommon-x11-dev libx11-xcb-dev \
  libssl-dev libzstd-dev libasound2-dev libdbus-1-dev \
  libvulkan1 mesa-vulkan-drivers xvfb xauth \
  openssh-client postgresql-client default-mysql-client
