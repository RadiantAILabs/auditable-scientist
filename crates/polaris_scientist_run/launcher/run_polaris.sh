#!/usr/bin/env bash
# Run one research campaign in a container.
#
# Usage: run_polaris.sh <label> [polaris-scientist run options...]
#
# Starts one container named polaris-sci-<label> from the project image and runs
#
#   polaris-scientist run /task-src --results-root <results root> <options...>
#
# inside it. Everything after the label is passed to `run` unchanged, so the
# caller chooses --max-usd, --config and any --set.
#
# Environment:
#   POLARIS_SCI_TASK_DIR  required; the task folder, mounted read-only at /task-src
#   POLARIS_SCI_OUT       required; the output root. The results root is
#                         $POLARIS_SCI_OUT/results/<label>, mounted at the same
#                         absolute path inside and outside the container
#   POLARIS_SCI_IMAGE     image to run (default: polaris-scientist:latest)
#   POLARIS_SCI_CPUS      CPU limit (default: 4)
#   POLARIS_SCI_MEMORY    memory limit (default: 12g)
#   AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, AWS_SESSION_TOKEN
#                         required; handed to the container by variable name only
#   AWS_REGION, AWS_DEFAULT_REGION
#                         handed over by name when set
#
# Exit status: the container's own status (0, 1, 2 and 4 are the binary's exit
# codes); 2 also for a usage error here, with no container started.
#
# On SIGTERM or SIGINT the launcher runs `docker stop --time 60` on the
# container, which gives the binary one cooperative stop signal and 60 seconds
# to finish before the container is killed.

# Never trace this script: the credential variables are read below, and a trace
# would print their values.
set +x
set -euo pipefail

fail() {
  echo "run_polaris: $*" >&2
  exit 2
}

[ "$#" -ge 1 ] || fail "usage: run_polaris.sh <label> [polaris-scientist run options...]"
label="$1"
shift
case "$label" in
  '' | [.-]* | *[!A-Za-z0-9_.-]*)
    fail "the label must be letters, digits, '.', '_' or '-' and start with a letter or digit"
    ;;
esac

missing=""
for name in AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN; do
  if [ -z "${!name:-}" ]; then
    missing="$missing $name"
  fi
done
[ -z "$missing" ] || fail "these credential variables are not set:$missing"

[ -n "${POLARIS_SCI_TASK_DIR:-}" ] || fail "POLARIS_SCI_TASK_DIR is not set"
[ -n "${POLARIS_SCI_OUT:-}" ] || fail "POLARIS_SCI_OUT is not set"
task_dir="$POLARIS_SCI_TASK_DIR"
out_root="$POLARIS_SCI_OUT"
image="${POLARIS_SCI_IMAGE:-polaris-scientist:latest}"
cpus="${POLARIS_SCI_CPUS:-4}"
memory="${POLARIS_SCI_MEMORY:-12g}"

task_dir="$(realpath -e -- "$task_dir" 2>/dev/null)" || fail "the task folder does not exist: $POLARIS_SCI_TASK_DIR"
[ -d "$task_dir" ] || fail "the task path is not a folder: $task_dir"
results_root="$(realpath -m -- "$out_root/results/$label")"
case "$task_dir$results_root" in
  *,*) fail "a path with a comma cannot be mounted" ;;
esac
mkdir -p -- "$results_root"

container="polaris-sci-$label"

# There is no --init. The container's first process holds the credentials in
# /proc/1/environ, which any command of the same user can read. With --init that
# process is docker-init; without it the first process is polaris-scientist, which
# marks itself unreadable before it reads the credentials (see hardening.rs).
args=(
  run --rm --sig-proxy=false
  --name "$container"
  --user "$(id -u):$(id -g)"
  --cap-drop ALL
  --security-opt no-new-privileges
  --pids-limit 1024
  --cpus "$cpus"
  --memory "$memory"
  --tmpfs /tmp:rw,nosuid,nodev,size=2g
  -e HOME=/tmp/home
  -e AWS_ACCESS_KEY_ID
  -e AWS_SECRET_ACCESS_KEY
  -e AWS_SESSION_TOKEN
  -e AWS_REGION
  -e AWS_DEFAULT_REGION
  --mount "type=bind,src=$task_dir,dst=/task-src,readonly"
  --mount "type=bind,src=$results_root,dst=$results_root"
  "$image"
  polaris-scientist run /task-src --results-root "$results_root"
  "$@"
)

stop_container() {
  docker stop --time 60 "$container" >/dev/null 2>&1 || true
}
trap stop_container TERM INT

# The container runs in the background so the trap runs as soon as a signal
# arrives. `--sig-proxy=false` keeps the docker client from forwarding the same
# signal: the binary must see exactly one stop signal, from `docker stop`.
docker "${args[@]}" &
client=$!

status=0
wait "$client" || status=$?
while kill -0 "$client" 2>/dev/null; do
  wait "$client" || status=$?
done
exit "$status"
