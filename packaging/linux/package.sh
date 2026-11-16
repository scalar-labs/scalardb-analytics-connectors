#!/bin/sh
# Builds the Linux distribution package with Docker, as CI does:
# packaging/linux/build.sh runs in the manylinux_2_28 image that
# packaging/linux/engine.sh pins, with the repository mounted. The package is
# for the Docker engine's own architecture; other architectures are built on
# their own hosts.
#
# Usage: packaging/linux/package.sh
#
# The package lands in target/dist, as packaging/package.sh writes it.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
# shellcheck source=packaging/linux/engine.sh
. "$root/packaging/linux/engine.sh"

exec docker run --rm -v "$root:/src" -w /src "$image" packaging/linux/build.sh
