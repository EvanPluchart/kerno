#!/usr/bin/env bash
# System packages needed to build and test Kerno on Debian/Ubuntu (GPUI's
# native dependencies, a headless X server for the UI tests, and the client
# tools Kerno calls for backup and restore). Run as root.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends \
  ca-certificates curl gnupg git pkg-config make cmake gcc g++ clang \
  libfontconfig-dev libwayland-dev libxkbcommon-x11-dev libx11-xcb-dev \
  libssl-dev libzstd-dev libasound2-dev libdbus-1-dev \
  libvulkan1 mesa-vulkan-drivers xvfb xauth \
  openssh-client postgresql-client default-mysql-client

# pg_dump refuses a server newer than itself, and the distribution's client
# can be older than the test server (PostgreSQL 17): the PostgreSQL 17 client
# of the PostgreSQL project's repository (apt.postgresql.org). Its key comes
# from the distribution's postgresql-common when it ships one, else from
# postgresql.org; either way it must have this fingerprint.
PGDG_FINGERPRINT=B97B0AFCAA1A47F044F244A07FCC7D46ACCC4CF8
key=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc
if [ ! -f "$key" ]; then
  key=/usr/share/keyrings/apt.postgresql.org.asc
  curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc -o "$key"
fi
found=$(gpg --show-keys --with-colons "$key" | awk -F: '$1 == "fpr" { print $10; exit }')
if [ "$found" != "$PGDG_FINGERPRINT" ]; then
  echo "The key of apt.postgresql.org in $key has the fingerprint $found, not $PGDG_FINGERPRINT." >&2
  exit 1
fi
codename=$(. /etc/os-release && echo "$VERSION_CODENAME")
echo "deb [signed-by=$key] https://apt.postgresql.org/pub/repos/apt $codename-pgdg main" \
  > /etc/apt/sources.list.d/pgdg.list
apt-get update
apt-get install -y --no-install-recommends postgresql-client-17
