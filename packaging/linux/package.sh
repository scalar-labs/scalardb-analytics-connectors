#!/bin/sh
# Builds the Linux distribution package with Docker, as CI does:
# packaging/linux/build.sh runs in the manylinux_2_28 image pinned here, with
# the repository mounted. The package is for the Docker engine's own
# architecture; other architectures are built on their own hosts.
#
# Usage: packaging/linux/package.sh
#
# The package lands in target/dist, as packaging/package.sh writes it.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)

engine_os=$(docker info --format '{{.OSType}}')
[ "$engine_os" = linux ] || { echo "the Docker engine runs $engine_os containers, not linux" >&2; exit 1; }
engine_arch=$(docker info --format '{{.Architecture}}')
case "$engine_arch" in
x86_64) image=quay.io/pypa/manylinux_2_28_x86_64@sha256:d086f64cddcace250ef0f0073583c51b8298159303290a730c9f59bd91edfd2c ;;
aarch64) image=quay.io/pypa/manylinux_2_28_aarch64@sha256:34a5e614a4eb26f73f8bf44cbbf2acf8011620453d03ea35a58537bb3cde44de ;;
*) echo "no manylinux image is pinned for the Docker engine's architecture $engine_arch" >&2; exit 1 ;;
esac

exec docker run --rm -v "$root:/src" -w /src "$image" packaging/linux/build.sh
