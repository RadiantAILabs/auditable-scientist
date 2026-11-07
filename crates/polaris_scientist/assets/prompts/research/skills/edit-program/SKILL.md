<!-- SPDX-License-Identifier: Apache-2.0; Copyright Sakana AI.
Source: AI-Scientist-v3 (df83138), src/agents/skills/edit-program/SKILL.md.
adapted: Editable clones use cp -r --no-preserve=mode; Trusted clone module replaced with available file operations; paths and candidate index follow the host-staged role; shell example removed; optional advisory tests use test_candidate in the same session. -->
---
name: edit-program
description: Edit a program for next experimentation.
---

# What the skill is about
Create the next program variant safely for iterative optimization experiments.

# When to use the skill
- After reviewing the latest evaluation results (or at the very beginning).
- When you want to test a new idea without overwriting previous attempts.

# What to do
1. Use the exact candidate index `<idx>` and destination named in the role instructions. Do not discover or create an additional candidate index.
2. Clone the previous staged program using file read/write tools or `cp -r --no-preserve=mode`.
   Copy the complete staged seed, including declared resource files, excluding
   `results/`, runtime caches, metrics, correctness,
   rubric reports, evaluation manifests and logs. The host checks append-only
   candidate indices and owns scoring; no upstream Python module is installed.
4. Edit code under `{{RESULTS_DIR}}/program_<idx>/src/**` (and update `program_spec.toml` if you change entrypoint or language).
5. Keep all older `program_<idx>` directories unchanged.
6. Never edit `{{TASK_DIR}}/evaluate.py` or `{{TASK_DIR}}/capture.py`.
7. When previous evaluation artifacts are staged, inspect the host-produced
`results/metrics.json`, `results/correct.json` and `results/rubric_status.json`
before editing. The host owns these files; do not create replacements.
8. If latest rubric status has `overall_pass: true`, stop; do not create another program directory.
9. When the role permits it, choose `test_candidate` on the completed output candidate.
   Read its advisory metrics, correctness, and logs before deciding to edit again or
   finish. At most one advisory test is available in this call. A failed or
   timed-out test consumes that allowance. Record what the evidence establishes
   and finish after any evidence-backed correction.
   Keep evaluator outputs in tests/, outside the candidate. The host always scores
   the final candidate independently. Do not run the evaluator through the shell.
10. Single-attempt official evaluation cap: never create/evaluate beyond `program_{{MAX_PROGRAM_EVALS_PER_ATTEMPT}}`.

# Example usage
Use file tools to create the next candidate's source tree and metadata.

Frozen staged inputs are read-only. To create an editable candidate, use
`cp -r --no-preserve=mode` when cloning into outputs/. Plain `cp -r`
preserves frozen modes and prevents edits. Never change input permissions.
