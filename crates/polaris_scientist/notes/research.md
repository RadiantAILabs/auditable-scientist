# Research implementation notes

The task, investigation, ablation and post-hoc workflows are composed graphs.
Model roles use the shared agent block; evaluator execution, mechanical rubrics,
selection, memory validation and artifact guards are systems. Investigation uses
one durable lease pool across workers. Variants and analyses use sequential
worklists, and bounded repair loops expose their configured caps.

The task evaluator is executed unchanged by a system, with a 300 second deadline
and five seconds of grace. A Linux child subreaper starts it in a private process
group, kills descendants and reaps adopted grandchildren, including on cancellation.
After timeout or cancellation, cleanup allows at most one further second before a
hard group kill. Candidate and scorer hashes are checked before and after the
invocation. Matching completed results are adopted; failed, partial or mismatched
results cannot trigger another evaluation of the same candidate.

The evaluator is independent of the agent's shell. No model has been run to
measure whether the shell's 120 second limit affects program quality. Offline
timeout tests use shorter explicit limits; the synthetic fixture retains its
own 300 second subprocess limit. Timeout and cancellation tests assert that
both the scorer and its descendant disappear from `/proc`, including zombies.

The shipped scorer makes a single observation despite `NUM_REPLICATIONS`
(`examples/circpack/evaluate.py:228-237` in SakanaAI/AI-Scientist-v3 df83138).
Requested replication counts must remain distinct from observed sample counts.
The variant manifest and summary record both. Fixture scores are synthetic
test values, never circle-packing measurements.

Task snapshots are immutable and retain scaffold hashes. The managed task-local
virtual environment checks exact dependency pins, tries the wheelhouse without
an index first, and allows index fallback only for a missing distribution.
It also provides the packages named by `evaluation.extra_packages` (default
`matplotlib` and `pandas`) from the wheelhouse alone, with the pins as
constraints; the installed versions are recorded in `task/runtime.json`. The
original's agents installed these at run time with one `pip install`; ours have
them preinstalled because agents here run without a network.
Offline tests use an empty dependency file and deliberately incorrect installed
pin metadata; they perform no package downloads.

Completed agent calls and canonical investigation receipts use durable journal
records. State hydration and integrity checks rerun on restart; evaluator calls
adopt matching evidence. Dynamic journal keys distinguish each lease, program and
step. Worklist joins reload prior item manifests. An interrupted author leaves its
lease pending and publishes no candidate or attempt receipt; resume uses a new
invocation workspace. Terminal lane failures remain failures after restart.

Promotion is independent of attempt-rubric success and requires a correct,
finite numeric score strictly above the incumbent. Ties retain the earlier
candidate, including the lower worker index at the join. Mechanical rubrics use
the upstream Python token/AST checker. Semantic rubrics remain `not_assessed`
unless the optional judge supplies evidence; they never govern promotion.

Prompt adaptations are listed in the header of each affected file:

- `idea.md`: replace an unavailable conclusions-helper invocation with file-tool
  writes under the same one-paragraph and 300-word rules.
- `harness_edit.md`: use the staged seed directory and append-only output
  candidate paths rather than the campaign filesystem.
- `variant_author.md`: extract the inline candidate-author text from
  `src/variants_pipeline/execute.py:643-656`, changing Python interpolation to
  named template variables and using private input/output paths.
- `posthoc_agent.md`: distinguish the frozen `inputs/bundle` from writable
  `outputs`, require workspace-relative evidence paths, state how manifest
  paths are written, state that the primary metric tolerance is a number, and
  state that each rubric entry carries an `evidence` sentence.
- `skills/edit-program/SKILL.md`: replace the upstream-only clone module with
  available file operations, use the host-assigned candidate index and staged
  paths, and remove its command example. Seeds retain supporting data while
  excluding caches and results; previous evaluation artifacts are staged under
  `results/` for inspection.
- `skills/verify-rubrics/SKILL.md`: read a staged rubric-status template,
  write a separate output status file, and inspect staged candidate results.
- `skills/memory-log/SKILL.md`: replace configuration-directory discovery with
  the staged skill path. Roles without shell use its existing file-tool path.

## Differences from the original (ideation and post-hoc)

The idea and analysis roles use the original's prompt text and make one call
where the original makes one. The differences that remain, each with its reason:

1. **Roles start from their instructions.** The idea and analysis calls send
   the role prompt and a one-line request; the prompt already holds the task,
   the histories and the incumbent's summary. The request has no reading list
   for the staged skills and inputs, and the original has none. A reading list
   is not what makes the model read: it still opens the staged incumbent
   program and task file before it writes.
2. **Ideation reads before it writes.** The original's idea agent wrote its
   files in one shell command from the prompt alone. This system's idea role has
   no shell and writes each file with a file tool, and it first reads the
   staged task, incumbent and conclusions. Compared on one recorded run of the
   original: the original used 2 turns per call and 4,171 output tokens in all; here the
   first call used 6 turns and 4,683 output tokens, and the retry after a
   rejected bundle used 9 turns and 5,369. A variant that named the staged
   files in the request used 5 requests and 4,989 output tokens for one call, so
   it was not kept. One sample each; the remaining gap is the model's choice to
   read the incumbent, which a prompt in the original's words does not forbid.
3. **The cycle block names files that are not all staged.** The prompt's
   `ideas_history_file`, `attempts_history_file` and `best_manifest_file` name
   paths under `inputs/`. Only files that exist are staged, so on the first
   cycle the idea agent can try to open a history file that is not there. The
   same facts are in the prompt text.
4. **The analyst has no heredoc and no compound command.** The agent shell runs
   one allowed command at a time, without pipes, heredocs or `&&`, for the
   reasons in the command restrictions. A heredoc patch is denied here, and the
   analyst then writes the file with a file tool, which costs a turn.
5. **The tolerance is a number.** `posthoc_agent.md` states that the primary
   metric tolerance is a number and that each rubric entry has an `evidence`
   sentence. Without the line, the analyst wrote a tolerance as text and the
   manifest was rejected, which cost a second analyst call.
6. **Figures use packages that are already installed.** The original's analyst
   installed `matplotlib` and `pandas` at run time with one `pip install`.
   Agents here have no network, so both are installed in the task environment
   from the wheelhouse before the run (`evaluation.extra_packages`).

Measured on one recorded post-hoc phase of the original (one sample each).
Without the tolerance line and without the packages: 2 analyst calls, 23 turns,
22,509 output tokens. With both: 1 call, 10 turns, 8,930 output tokens. The
original: 1 call, 6 turns, 4,303 output tokens. The proposer and the selector
are close to the original's. The rest of the gap in the analyst is exploratory
commands and a different analysis chosen, which one sample cannot separate from
run-to-run difference.

Other vendored research prompts retain their upstream text. Every vendored
Markdown file carries its source, licence identifier and edit record, alongside
an Apache-2.0 licence and NOTICE. The token/AST rubric checker is copied unchanged
from `src/investigation/rubric_checks.py:1-330`, with its attribution header.

The author grants preserve the upstream harness restriction on executing Python,
tests or candidate code (`src/prompts/harness_edit.md:45-53`). This is stricter
than allowing exploratory program execution: only the scoring system executes
the candidate. Retrofit checks enforce scorer integrity and entrypoint validity;
the subsequent required `control_best` evaluation checks runtime correctness,
avoiding an additional official scoring call outside the three evaluator sites.

The final phase records missing contracts as degraded and present contracts as
unsupported. It does not synthesize a contract or implement sealed evaluation.
Post-hoc analysis never selects a winner or reruns the evaluator. Checkpoint
bootstrap, pruning/round scheduling and task drafting remain explicitly
unbuilt; drafting returns `NotBuilt`, while requested pruning and checkpoint
bootstrap write skip evidence. Frozen analyses must produce grounded manifests,
artifact-only rubrics and resolving evidence paths without new program variants.
When the proposer writes a path as the analysis workspace shows it, the selector
removes the `inputs/bundle/` prefix from a required artifact and the `outputs/`
prefix from an expected output, then applies the usual safety and existence checks
to the result; a path that leaves the bundle after the prefix is removed is rejected.

The research test target covers task provisioning, immutable scoring, process
cleanup, promotion/ties, rubrics, memory, caps, lane failures, interruption/resume,
sequential variants, frozen analyses, budget refusal, role grants and complete
main/skill rendering. It uses scripted responses and local temporary folders,
with no model, credential, network or container access. Both parallel and serial
test execution are required; formatting and strict Clippy are checked separately.
