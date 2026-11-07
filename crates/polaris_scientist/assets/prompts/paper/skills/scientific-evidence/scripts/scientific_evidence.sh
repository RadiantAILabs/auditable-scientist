#!/usr/bin/env bash
# Source: AI-Scientist-v3 src/agents/skills/scientific-evidence/scripts/scientific_evidence.sh (Apache-2.0; see LICENSE and NOTICE in the folder assets/prompts/paper).
# adapted: none
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$SCRIPT_DIR/scientific_evidence.py" "$@"
