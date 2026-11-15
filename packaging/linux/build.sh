#!/bin/sh
# Builds the Linux distribution package inside a manylinux_2_28 container, so
# that the shipped binaries need no glibc newer than 2.28. It runs in the
# container that packaging/linux/package.sh starts, which pins the image.
#
# The image provides GCC from gcc-toolset and CMake; this script installs the
# rest of what packaging/package.sh requires, at the versions of the Nix
# development shell, then runs it.
set -eu

rust_version=1.99.1
cargo_about_version=0.9.0

dnf install -y -q jq unzip openssl
export CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup PATH="/usr/local/cargo/bin:$PATH"
curl -fsSL https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain "$rust_version" --no-modify-path
cargo install -q --locked cargo-about --version "$cargo_about_version" --features cli

# protoc for the Rust Substrait types.
case "$(uname -m)" in
x86_64) protoc_arch=x86_64 protoc_sha256=af27ea66cd26938fe48587804ca7d4817457a08350021a1c6e23a27ccc8c6904 ;;
aarch64) protoc_arch=aarch_64 protoc_sha256=31c5e9e3c7bf013cf41fb97765ee255c140024a6b175b6cc9b64beddd7c23ba7 ;;
esac
curl -fsSL -o /tmp/protoc.zip \
	"https://github.com/protocolbuffers/protobuf/releases/download/v34.1/protoc-34.1-linux-$protoc_arch.zip"
echo "$protoc_sha256  /tmp/protoc.zip" | sha256sum -c -
unzip -q -o -d /usr/local /tmp/protoc.zip bin/protoc 'include/*'

# Kept apart from the host's build outputs in a mounted repository.
CARGO_TARGET_DIR="$(pwd)/target/manylinux"
export CARGO_TARGET_DIR
exec "$(dirname "$0")/../package.sh"
