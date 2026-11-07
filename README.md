<p align="center">
  <img src="assets/auditable-scientist-logo.svg#gh-light-mode-only" width="716" alt="Auditable Scientist">
  <img src="assets/auditable-scientist-logo-dark.svg#gh-dark-mode-only" width="716" alt="Auditable Scientist">
</p>

<p align="center">
  <strong>An AI scientist you can audit. Its rules are code, and every call is on record.</strong>
</p>

<p align="center">
  <a href="https://radiantlabs.dev"><img src="https://img.shields.io/badge/website-radiantlabs.dev-000000?labelColor=000000&amp;style=flat-square" alt="Website: radiantlabs.dev"></a>
  <a href="https://github.com/RadiantAILabs/auditable-scientist/tags"><img src="https://img.shields.io/github/v/tag/RadiantAILabs/auditable-scientist?label=release&amp;color=000000&amp;labelColor=000000&amp;style=flat-square" alt="Latest release tag"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-000000?labelColor=000000&amp;style=flat-square" alt="License: Apache 2.0"></a>
</p>

<p align="center">
  <a href="https://radiantlabs.dev"><strong>Website</strong></a> ·
  <a href="https://radiantlabs.dev/blog/agentic-scientist-as-a-graph"><strong>Blog post</strong></a> ·
  <a href="#get-started"><strong>Get started</strong></a> ·
  <a href="#result"><strong>Result</strong></a> ·
  <a href="records/README.md"><strong>Records</strong></a> ·
  <a href="https://github.com/RadiantAILabs/polaris"><strong>Polaris framework</strong></a>
</p>

Auditable Scientist rebuilds Sakana AI's
[Agentic Scientist](https://arxiv.org/abs/2610.18933)
([AI-Scientist-v3](https://github.com/SakanaAI/AI-Scientist-v3)) as a
[Polaris](https://github.com/RadiantAILabs/polaris) graph. Each model call runs
as its own session of [Polaris Code](https://github.com/RadiantAILabs/polaris_code).

AI-Scientist-v3 takes a research task and returns a finished paper. It
proposes ideas, writes and scores programs, runs ablations, analyses the
results and writes the paper. The original hands each stage to a general coding
agent with a full shell. Here, every model call is one node in a graph. The
rules around the call are code: what it can read, what it can run, what it can
spend, and how its output is checked. After a run, anyone can see what each
step was allowed to do, read what it did, and trace each number in the paper
to a saved result.

On the circle packing task, with the same model, the graph scored 2.630 to the
original's 2.636, and both papers got the same review score. See the
[result](#result) for the measurements and scope, and the
[blog post](https://radiantlabs.dev/blog/agentic-scientist-as-a-graph) for the
design.

## Why Auditable Scientist

- **Rules in code, not in prompts.** Each role gets its own folders, a fixed
  list of commands and a dollar limit. No role has an unrestricted shell.
- **A score the model cannot reach.** Only a step of the graph runs the task's
  evaluator, and it checks that the program and the scorer did not change.
- **Every call on record.** Each model call keeps its full conversation: 33 of
  33 in the reported run, against 33 of 56 in the original.
- **Every number traced.** The paper does not pass until every number it
  prints is found in a saved result: 116 of 116, against 62 of 143.
- **Loops that end.** Checks and repairs are edges in the graph with a fixed
  limit, so a campaign cannot loop forever.
- **The same result.** Same task and model, nearly the same score, and the same
  overall review score.

## Result

We ran the original and the graph once each on one task, circle packing, with
the same model (Claude Sonnet 5.5) on both sides.

| Measure | Original | Polaris graph |
|---|---|---|
| Best score, re-scored by the task's own evaluator | 2.636 | 2.630 |
| Paper | 13 pages, compiles | 13 pages, compiles |
| Automated reviewer, overall score | 2 (reject) | 2 (reject) |
| Paper's own evidence audit | failed after 11 rounds | passed in round 4 |
| Printed numbers backed by a saved result | 62 of 143 | 116 of 116 |
| Model calls with every message and tool call saved | 33 of 56 | 33 of 33 |
| Roles given an unrestricted shell | all | none |
| Wall-clock time, one worker | 75 min | 57 min |
| Token cost, from the recorded usage | $9.09 | $9.01 |

This is one task and one run for each side. It shows that the graph can
reproduce the original's result on this task, and that the graph can enforce
rules the original leaves to the agent. It does not show that the graph is
faster or cheaper: the two runs differ in search time and in the number of
audit rounds. The run records, and the scripts that recompute the numbers in
this table, are in [records/README.md](records/README.md).

## Get started

A campaign calls a model and costs money. Set a dollar limit that you accept.

### Build

You need Rust 1.97.0 (`rustup toolchain install 1.97.0`) and a C compiler.

```sh
cargo +1.97.0 build --release
```

The binary is `target/release/polaris-scientist`. The `polaris_code` crates are
fetched from GitHub at a fixed commit.

### Build the images

1. **The base image.** The image of the original has Debian 13, Python 3.12,
   `pdflatex` and `bibtex`. Build it from a clone of AI-Scientist-v3 at commit
   `df83138`:

   ```sh
   git clone https://github.com/SakanaAI/AI-Scientist-v3 original
   git -C original checkout df83138
   docker build -t agentic-scientist/agent-cli:latest \
     -f original/src/docker/agent-cli/Dockerfile original
   ```

2. **The project image.** This builds the binary and the image around it. It
   needs the network once, to fetch the Python packages the task uses.

   ```sh
   crates/polaris_scientist_run/image/build.sh
   ```

### Run a campaign

1. **The task.** Use the `examples/circpack` folder of the same clone. It holds
   the circle packing task. The launcher mounts it read-only.

2. **The settings.** The launcher reads these environment variables:

   ```sh
   export POLARIS_SCI_TASK_DIR="$PWD/original/examples/circpack"
   export POLARIS_SCI_OUT="$PWD/out"
   ```

   The model keys must be in your environment too: `AWS_ACCESS_KEY_ID`,
   `AWS_SECRET_ACCESS_KEY` and `AWS_SESSION_TOKEN`. Take them from your shell or
   a secret manager. Never put them in a file in this repository. The launcher
   passes them to the container by name only and prints no value. Without them
   it starts nothing.

3. **The model and the region.** The model is Claude Sonnet 5.5 on Amazon
   Bedrock, in `eu-west-2`. To change them, put the values in shell variables
   and pass them as settings:

   ```sh
   model="aws.bedrock/global.anthropic.claude-sonnet-5-5"
   region="eu-west-2"
   ```

4. **Start.** The first argument is a label for the run. The launcher passes
   the rest to `polaris-scientist run`. The reduced configuration is a short
   run of the circle packing task.

   ```sh
   crates/polaris_scientist_run/launcher/run_polaris.sh my-run \
     --max-usd 13.5 \
     --config /opt/polaris-scientist/configs/circpack_reduced.toml \
     --set "runner.model=\"$model\"" \
     --set "runner.region=\"$region\""
   ```

   The results are in `$POLARIS_SCI_OUT/results/my-run`. The exit codes, the
   `resume` and `reset` commands and the container limits are in
   [crates/polaris_scientist_run/README.md](crates/polaris_scientist_run/README.md).

## How it works

A campaign is one graph with six phases. Code does the setup, the scoring and
the checks. Only the roles in the third column call a model.

| Stage in the original | Phase in the graph | Roles that call the model |
|---|---|---|
| Set up the task | `task_drafting`: copies the given task, pins its environment and scores the seed program. Writing a new task is not built. | None |
| Propose ideas, write programs | `investigation` | `ideation`, `program_author`, and the `rubric_verifier` (off by default) |
| Ablations | `ablations` | `variant_propose`, `variant_select`, `variant_retrofit`, `variant_author`, `variant_verifier`, `variant_summarizer` |
| Final evaluation | `final_evaluation`: not built. The phase records why it did not run. | None |
| Post-hoc analysis | `posthoc` | `posthoc_proposer`, `posthoc_selector`, `posthoc_analyst` |
| Write the paper | `paper` | `reference_planner`, `paper_strategist`, `posthoc_ranker`, `paper_writer`, `citation_gap_analyst`, `reference_planner_postdraft`, `paper_enricher`, `evidence_auditor`, `paper_repairer`, and the `presentation_reviewer` (off by default) |
| Outside review | `review`: off by default. It is meant to run after the campaign, on the finished paper. | None |

Checks and repairs are edges in the graph. Each repair loop has a fixed
limit, so a campaign cannot loop forever. The prompts and skills come from the
original, with the changes listed in the NOTICE file next to them.

Not built: writing a new task, the sealed final evaluation, the revision after
an outside review, the concept figure, and the `scholarly` fallback for
references.

### What each role may do

The limits are in `crates/polaris_scientist/assets/roles/`. Every role has a
turn limit, a time limit and a dollar limit. The table shows the turn limit and
the dollar limit.

| Role | Commands | Writes files | Turns | Max USD |
|---|---|---|---|---|
| `ideation` | none | yes | 30 | 0.60 |
| `program_author` | set A | yes | 60 | 1.20 |
| `rubric_verifier` | none | yes | 20 | 0.40 |
| `variant_propose` | none | yes | 25 | 0.50 |
| `variant_select` | none | yes | 15 | 0.25 |
| `variant_retrofit` | set B | yes | 40 | 0.80 |
| `variant_author` | set A | yes | 40 | 0.80 |
| `variant_verifier` | none | yes | 20 | 0.40 |
| `variant_summarizer` | none | yes | 10 | 0.20 |
| `posthoc_proposer` | none | yes | 25 | 0.50 |
| `posthoc_selector` | none | yes | 15 | 0.25 |
| `posthoc_analyst` | set B | yes | 60 | 1.20 |
| `reference_planner` | none | yes | 15 | 0.30 |
| `paper_strategist` | none | yes | 20 | 0.50 |
| `posthoc_ranker` | none | yes | 15 | 0.25 |
| `paper_writer` | set C | yes | 80 | 2.50 |
| `citation_gap_analyst` | none | yes | 20 | 0.40 |
| `reference_planner_postdraft` | none | yes | 10 | 0.20 |
| `paper_enricher` | set C | yes | 40 | 0.80 |
| `evidence_auditor` | set D | no | 30 | 0.60 |
| `paper_repairer` | set C | yes | 60 | 1.50 |
| `presentation_reviewer` | none | no | 25 | 0.50 |

- Set A: `ls`, `cat`, `rg`, `cp`, `mkdir`, `diff`, `grep`, `head`, `tail`, `wc`,
  `sort`, `uniq`.
- Set B: set A, and `python`, `python3`, `cd`.
- Set C: set B, and `pdflatex`, `bibtex` and `bash skills/*`, which runs the
  scripts of the skills staged for the role.
- Set D: set B without `cp` and `mkdir`.

No role has an unrestricted shell. A role with "none" runs no command at all.
"Writes files" means the role may use the agent's file tools to create and edit
files in its call folder.

### How the graph enforces this

- **Folders.** Each call runs in its own folder. The task and the earlier
  results are copied in under `inputs/`, read-only. The role writes its results
  under `outputs/`. A role with `Writes files: no` gets no permission to edit at
  all.
- **Commands.** The runner gives the role's command list to the agent's
  permission policy. A command that matches none of the patterns is refused, and
  the agent is told so. A command runs with an empty environment that holds only
  `PATH`, `VIRTUAL_ENV` and a few harmless names. It is limited in time and in
  output.
- **Scoring.** Only a step in the graph runs the task's evaluator, and its
  result is the score. The step runs the evaluator with a time limit and checks
  the hashes of the program and the scorer before and after. The author roles
  can try their program with one `test_candidate` call. That test is advisory,
  runs without network access and can write only in its own folder.
- **Money.** A campaign has one total, set with `--max-usd`. Each phase may
  spend up to a cumulative share of it. Before a call starts, the graph reserves
  the role's dollar limit from that share. If less than a quarter of the limit
  is available, the call is not made. During the call, a meter prices each
  request and refuses one that could pass the limit. When the call ends, the
  real cost is settled and the rest is released. A call that is lost in a crash
  is charged its full limit when the campaign resumes.
- **Turns and time.** The turn limit and the time limit of the role end the call
  when they run out. A campaign can also have a wall-clock limit
  (`budget.wall_seconds`).
- **Credentials.** The model keys are read once, when the runner starts. They
  are never passed to a command and never written to a file.
- **Numbers in the paper.** The claims check looks up every number the paper
  prints in the files the run saved. A number with no recorded value or recorded
  calculation is an error, and the paper goes back to a repair step.

### How records are kept

A campaign writes one results folder. It holds:

- `run.json`, `config.resolved.toml` and `roles.resolved.toml`: the identity of
  the run, the settings it used and the limits of each role.
- `pipeline_events.jsonl`: one line for each event of the campaign.
- `metrics/cost_events.jsonl`: one line for each model call, with its tokens and
  its cost. `metrics/spend_state.json` holds the running totals.
- `journal.jsonl`: the steps that have finished. A resume skips them.
- `work/<step>/001`, `002`, and so on: one folder for each model call. It holds
  the role prompt, the task prompt, the inputs, the outputs and
  `transcript.jsonl`, the whole conversation of the call. A step that was
  interrupted is repeated in a new folder, and the old one moves to
  `work/_abandoned/`.
- One folder for each phase (`investigation`, `ablations`, `posthoc`, `paper`,
  and so on) with the files it produced.
- `outcome.json`: how the campaign ended, what it spent, which limits it hit and
  which steps it skipped.

`polaris-scientist check-claims` reruns the number check on a finished paper. It
uses no model, no network and no credential.

## Documentation

| Document | What it covers |
|---|---|
| [crates/polaris_scientist/README.md](crates/polaris_scientist/README.md) | The campaign library and its modules. |
| [crates/polaris_scientist_run/README.md](crates/polaris_scientist_run/README.md) | The `polaris-scientist` command, its settings, the launcher and the image, and how a role call ends. |
| [notes/research.md](crates/polaris_scientist/notes/research.md) | Research implementation notes. |
| [notes/ablations.md](crates/polaris_scientist/notes/ablations.md) | How ablation variants are checked. |
| [notes/evidence.md](crates/polaris_scientist/notes/evidence.md) | The paper phase, the review phase and the claims check. |
| [records/README.md](records/README.md) | The run records behind the result, and how to check them. |
| [NOTICE](NOTICE) | The files that come from AI-Scientist-v3, and what changed. |

## Development

```sh
cargo +1.97.0 fmt -- --check
cargo +1.97.0 clippy --all-targets --all-features -- -D warnings
cargo +1.97.0 test --workspace
```

No test calls a model, uses a credential, reaches the network or starts a
container. The model is a scripted stand-in. A stand-in `docker` and a stand-in
`pdflatex` replace the real programs.

The tests need:

- Linux. The tests of the `test_candidate` tool run a program under Landlock and
  seccomp. They need Landlock ABI 3 or later, which is Linux 6.2 or later with
  Landlock turned on.
- `python3` with `venv` and `pip`. Some tests build a small virtual
  environment from a wheel they write themselves.
- `bash` and the usual command line tools (`cp`, `cat`).
- A C compiler. The build needs one.

The tests have been run on one Linux machine only. This repository has no
automatic test run on GitHub yet.

| Path | Purpose |
|---|---|
| `crates/polaris_scientist` | The campaign graph, the settings, the roles, the prompts, the evidence checks and the paper tools. |
| `crates/polaris_scientist_run` | The runner that gives each role call to a `polaris_code` agent session, the `polaris-scientist` command, the launcher scripts and the container image. |
| `records/` | The run records and checks behind the result above. |
| `assets/` | The logo. |

## Contributing

Open an [issue](https://github.com/RadiantAILabs/auditable-scientist/issues)
for a bug report or a question, or send a pull request with a focused change.
Include the checks under [Development](#development) when you submit code.

## Credit

This work builds on AI-Scientist-v3 by Sakana AI, which is published under the
Apache License 2.0. Many prompts, skills and the paper template come from it.
Thank you to the Sakana AI team. See [NOTICE](NOTICE) for the list of files and
changes.

## License

Apache License 2.0. See [LICENSE](LICENSE).

Built by [Radiant Labs](https://radiantlabs.dev).
