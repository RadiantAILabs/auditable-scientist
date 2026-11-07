#!/usr/bin/env bash
# Check a paper's claims against recorded evidence in a locked-down container.
#
# Usage: run_claims_check.sh <paper dir> <out file>
#
# Runs `polaris-scientist check-claims` with no network, no credentials and a
# read-only view of the paper folder and of the image. The report is printed by
# the program on its standard output and written to <out file> by this script,
# so nothing in the container is writable except a small temporary folder.
#
# Environment:
#   POLARIS_SCI_IMAGE  image to run (default: polaris-scientist:latest)
#
# Exit status: the container's own status; 2 for a usage error, with no
# container started.

set +x
set -euo pipefail

fail() {
  echo "run_claims_check: $*" >&2
  exit 2
}

[ "$#" -eq 2 ] || fail "usage: run_claims_check.sh <paper dir> <out file>"
paper_dir="$(realpath -e -- "$1" 2>/dev/null)" || fail "the paper folder does not exist: $1"
[ -d "$paper_dir" ] || fail "the paper path is not a folder: $paper_dir"
out_file="$(realpath -m -- "$2")"
case "$paper_dir" in
  *,*) fail "a path with a comma cannot be mounted" ;;
esac
image="${POLARIS_SCI_IMAGE:-polaris-scientist:latest}"

mkdir -p -- "$(dirname -- "$out_file")"
partial="$out_file.partial.$$"
trap 'rm -f -- "$partial"' EXIT

status=0
docker run --rm --init --network none --read-only \
  --user "$(id -u):$(id -g)" \
  --cap-drop ALL --security-opt no-new-privileges \
  --pids-limit 512 --cpus 2 --memory 4g \
  --tmpfs /tmp:rw,nosuid,nodev,size=512m \
  -e HOME=/tmp \
  --mount "type=bind,src=$paper_dir,dst=/paper,readonly" \
  "$image" \
  polaris-scientist check-claims --paper-dir /paper --out - >"$partial" || status=$?

if [ "$status" -eq 0 ]; then
  mv -f -- "$partial" "$out_file"
fi
exit "$status"
