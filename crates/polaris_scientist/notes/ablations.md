# Controlled variant validation

A comparison candidate must finish authoring successfully and change executable
source before the evaluator runs. Editing metadata, documentation, or saved
metrics alone does not establish an intervention. The author gate compares the
candidate with its original control, including after a previous candidate has
been evaluated. See `author_check` in `src/phases/ablations/mod.rs` and
`require_source_change` in `src/phases/ablations/contracts.rs`.

Incomplete or unchanged authoring has its own graph repair loop, with an initial
attempt and two repairs per candidate. Its validation reason becomes the next
author's previous-status context. Exhausting this loop records a failed variant
with zero additional evaluations and no score for the rejected candidate. The
separate configured evaluation cap is unchanged. This follows the original's
incomplete-author retry bound (`AI-Scientist-v3` at `df83138`,
`src/variants_pipeline/execute.py:47,419`), adding successful-stop and source-change
requirements before scoring.

Frozen `inputs/` remain read-only. Ordinary `copy_tree` snapshots preserve
source file modes. Explicit `copy_editable_tree` copies restore owner read/write
access, and owner traversal access on directories. Role instructions and the
edit-program skill require `cp -r --no-preserve=mode` for shell-created editable
clones; plain `cp -r` preserves the frozen modes. This applies to investigation,
variant authoring, instrumentation and other research role calls through the
shared call builder. The source input's modes and bytes remain unchanged.

Every materialized variant has the original's nine rubric families and the three
additional reference-adaptation checks when applicable. Proposal-supplied rubrics
cannot substitute a metric-only contract. The canonical families and CSV schemas
come from `AI-Scientist-v3` at `df83138`,
`src/variants_pipeline/constants.py:9,48` and `rubric_contract.py:19,73`.
The instrumentation role receives the complete column schemas.

A repair starts from the previous evaluated candidate, so its copy can carry that
candidate's `results/` folder. The host removes the evaluator's files from every
newly copied candidate before scoring; otherwise the evaluator refuses the copy as
a fingerprint mismatch and the repair is never scored.

The host checks analysis-file presence, the three CSV schemas, score/primary-metric
agreement with evaluator metrics, and recorded correctness. The primary metric is
compared with the evaluator's value of the same name when the evaluator reports one
at the top level of `metrics.json`; a name the evaluator does not report (for
example the quantity a score sums) stands for the evaluator's `combined_score`.
Header-only timeseries are valid. CSV files are bounded and cannot resolve through symlinks.
The verifier role judges intervention intent, single-change scope and analysis
readiness using the specification, candidate, original control and recorded
results. It must cite source evidence; equal scores cannot establish that a
removal happened. Host checks overwrite any conflicting model assessments.

The host enforces exact trusted rubric IDs, boolean decisions, bounded nonempty
evidence and a bounded optional recommendation. It writes identities, attempt
IDs, metrics and overall pass itself. These constraints follow the original's
canonicalizer (`src/variants_pipeline/rubric_contract.py:194,242`); output identity
or aggregate claims from the model are ignored.

Prompt differences from the original's inline author and verifier instructions
(`src/variants_pipeline/execute.py:643,658`) are the private `inputs/` and
`outputs/` transport, explicit writable-copy command, source comparison against
the staged control, and prefilled mechanical assessments that the model must
preserve. The existing vendored verify-rubrics skill remains the verifier prompt;
role-specific comparison instructions accompany the call. No evaluator,
objective, search schedule, replication count or evaluation cap changes.

Offline tests in `tests/research/ablations.rs` and `tests/research/topology.rs`
exercise frozen clone modes, unchanged candidates, successful repair, failed
stops with candidate files present, trusted rubric IDs, forged model fields,
mechanical override of incorrect analysis, a repair that carries the previous
candidate's results, a primary metric name the evaluator does not report, and the
edit/evaluation bounds.
