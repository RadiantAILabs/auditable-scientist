#!/usr/bin/env bash
# Build the release binary from the current tree and the project image around it.
#
# Usage: image/build.sh [image tag]
#
# The tag defaults to polaris-scientist:latest. The build needs the base image
# (agentic-scientist/agent-cli:latest) locally and, for the wheelhouse, a package
# index to fetch numpy and scipy wheels from. The base image is built from
# src/docker/agent-cli/Dockerfile of AI-Scientist-v3 at commit df83138; the
# README of this crate has the commands.
#
# The binary is built on this machine and must run on the base image's C
# library. The build ends by running `polaris-scientist --version` in the new
# image and fails if that does not work; in that case build the binary inside a
# Debian 13 container, with the same cargo command, and copy it into the context.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
crate="$(dirname "$here")"
tag="${1:-polaris-scientist:latest}"

cd "$crate"
cargo +1.97.0 build --release -p polaris_scientist_run --bin polaris-scientist

target_dir="$(cargo +1.97.0 metadata --format-version 1 --no-deps |
  python3 -I -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"

context="$(mktemp -d)"
trap 'rm -rf -- "$context"' EXIT
mkdir -p "$context/configs"
cp -- "$target_dir/release/polaris-scientist" "$context/polaris-scientist"
cp -- "$crate/configs/circpack_reduced.toml" "$context/configs/circpack_reduced.toml"
cp -R -- "$(dirname "$crate")/polaris_scientist/assets" "$context/assets"

docker build --tag "$tag" --file "$here/Dockerfile" "$context"

# The binary must resolve its libraries and start on the image's C library.
docker run --rm --entrypoint polaris-scientist "$tag" --version
