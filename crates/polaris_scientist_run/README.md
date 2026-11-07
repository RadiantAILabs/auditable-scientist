# polaris_scientist_run

Runs the research campaign of `polaris_scientist` on the `polaris_code` agent.

- `SessionRunner` runs each role call of a campaign as one agent session, with
  its own prompt, folder, tools, permissions, spend limit and transcript.
- `polaris-scientist` is the command that starts, resumes and resets campaigns.
- `launcher/` and `image/` run the command in a container.

## Build

```bash
cargo +1.97.0 build --release -p polaris_scientist_run
```

The binary is `target/release/polaris-scientist`. Checks:

```bash
cargo +1.97.0 fmt -- --check
cargo +1.97.0 clippy --all-targets --all-features -- -D warnings
cargo +1.97.0 test -p polaris_scientist_run
```

No test uses a real model, a real credential, the network or a container. The
model is a scripted stand-in, and the launcher tests put a stand-in `docker`
first on `PATH`.

## Commands

| Command | What it does |
|---|---|
| `run <task-dir> --results-root <dir> --max-usd <usd>` | Start a campaign. Options: `--config <file>` (repeatable, later files win), `--set key=value` (repeatable, applied after the files), `--run-id <name>`. The task directory is only read. |
| `resume --root <run-dir> --max-usd <usd>` | Continue an interrupted or failed run. `--set key=value` may change a setting, but not the shape of the campaign. |
| `reset --root <run-dir> --from <phase>` | Forget the work from a phase on, so a resume repeats it. |
| `check-claims --paper-dir <dir> --out <file>` | Check a finished paper against its recorded claims: every declared claim must resolve to recorded sources and citation keys (`unresolved_claims` lists each one that does not, with the reason), and every number the manuscript prints must be backed by a claim and a recorded value or derivation. The report keeps the fresh `mechanical_check` (with `passed`) apart from the `saved_model_audit`, which is copied from the saved audit file and not run again. The exit code says the check ran, not that it passed. `--out -` writes the report to standard output. Uses no model, no network and no credential. |
| `config --print [--config <file>] [--set key=value]` | Print the merged configuration as TOML. Needs no key. |
| `agent-call --results-root <dir> --workspace <dir> --role-prompt <file> --task <file> --max-usd <usd>` | Make one agent call outside a campaign, with the same agent, tools and spend limit as a role call, and print how it ended as JSON. Options: `--max-turns` (default 50), `--timeout-seconds` (default 900), `--edit` (allow file edits), `--command <glob>` (repeatable; with none, the agent runs no commands). Commands run in the Python environment `<results-root>/.venv`. |
| `--version` | Prints the crate version and source revision, the `polaris_code` version, and the target. |

`--max-usd` is a decimal number of dollars, such as `13.5`, with at most six
decimals. It is converted to whole micro-dollars. It is required for `run`,
`resume` and `agent-call`, and `0` is refused, unless the process is offline
(see "Environment").

`--set` uses the keys shown by `config --print`. An unknown key is refused.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | The work is done: the campaign outcome is `completed` or `completed_no_winner`, or the report was written. |
| 1 | The work ran and failed: the outcome is `failed` or `paper_incomplete`, or a command could not run to its end. |
| 2 | Nothing ran: the command line, a configuration value, a file named on the command line, the model provider's startup check, or a credential was unusable. The launcher also uses 2 when it refuses to start a container. |
| 4 | The campaign was interrupted and can be resumed. |

Any campaign outcome that is not named above counts as a failure, so a new
outcome is never reported as success.

### Signals

`SIGTERM` and `SIGINT` ask the campaign to stop. Calls in flight are
interrupted, no new call starts, and the campaign records where it stopped; the
exit code is 4 and `resume` continues from there. A second signal ends the
process at once with exit code 4, without waiting for the campaign.

### Time

The campaign keeps to `budget.wall_seconds`: once it has run out, no new role
call starts.

## Environment

| Variable | Effect |
|---|---|
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` | The model credentials. Read once, when the runner starts. Never passed to a command the agent runs, and never written to a log, a report, a transcript or a file. |
| `AWS_REGION`, `AWS_DEFAULT_REGION` | Not used to pick the region: the model is called in the region of the `runner.region` setting (default `eu-west-2`). The launcher still hands them over by name when they are set. |
| `POLARIS_SCIENTIST_OFFLINE` | `1` means no model can be reached: a scripted runner is used (`runner.kind = "scripted"`, `runner.scenario` names the scenario directory), `--max-usd` is optional, and the session runner is never built. Without it, `runner.kind = "scripted"` is refused. `agent-call` is refused while it is set. |
| `POLARIS_SCIENTIST_ASSETS` | The folder of prompts, roles and venue notes. The image sets it. Without it the campaign looks beside the binary and then in the source tree it was built from. |
| `HOME` | Created, for its owner only, if it does not exist. The launcher points it at an empty temporary folder. |
| `AWS_EC2_METADATA_DISABLED` | The image sets it to `true`. See "Credentials" below. |

The launcher, `launcher/run_polaris.sh <label> [run options...]`, reads these
as well. It passes everything after the label to `run`.

| Variable | Effect |
|---|---|
| `POLARIS_SCI_TASK_DIR` | Required. The task folder, mounted read-only at `/task-src`. |
| `POLARIS_SCI_OUT` | Required. The output root. The results root is `$POLARIS_SCI_OUT/results/<label>`; it is mounted at the same absolute path inside and outside the container. |
| `POLARIS_SCI_IMAGE` | The image to run. Default `polaris-scientist:latest`. |
| `POLARIS_SCI_CPUS`, `POLARIS_SCI_MEMORY` | Limits for the container. Default 4 and `12g`. |

## Launcher and image

`launcher/run_polaris.sh <label> --max-usd 13.5 --config /opt/polaris-scientist/configs/circpack_reduced.toml`
starts one container named `polaris-sci-<label>` that runs
`polaris-scientist run /task-src --results-root <results root> ...`. The
container runs as the host user's uid and gid, with all capabilities dropped,
`no-new-privileges`, a process limit and the CPU and memory limits above, and a
temporary `/tmp` of 2 GiB. The three key variables are required: without them
the launcher exits with 2 and starts nothing. They reach the container by
name (`-e NAME`), never as `NAME=value`, and no value is printed, even with
`bash -x`. `SIGTERM` or `SIGINT` to the launcher runs `docker stop --time 60`
on the container, which gives the binary one stop signal and 60 seconds to
finish. The launcher's exit status is the container's.

`launcher/run_claims_check.sh <paper-dir> <report-file>` runs `check-claims`
in a container with no network, no credential, a read-only root and a read-only
mount of the paper. The report is written to a partial file and moved into
place only when the check succeeds.

`configs/circpack_reduced.toml` holds the four settings of a short circle
packing run and nothing else.

### Image

`image/build.sh [tag]` builds the release binary from the current tree and then
the image, and ends by running `polaris-scientist --version` in it. The image
starts from `agentic-scientist/agent-cli:latest` (Debian 13, Python 3.12,
`pdflatex`, `bibtex`), which must already be on the machine. Build that base
image from the Dockerfile of AI-Scientist-v3, at commit `df83138`:

```sh
git clone https://github.com/SakanaAI/AI-Scientist-v3 original
git -C original checkout df83138
docker build -t agentic-scientist/agent-cli:latest \
  -f original/src/docker/agent-cli/Dockerfile original
```

The image holds the
binary, the campaign assets, the reduced configuration, and a wheelhouse in
`/opt/wheelhouse` with the `numpy==2.4.2` and `scipy==1.17.1` wheels, so a run
installs them with `pip install --no-index --find-links /opt/wheelhouse
numpy==2.4.2 scipy==1.17.1`. The wheelhouse also holds `matplotlib==3.11.2` and
`pandas==3.1.1` with their dependencies. When a run creates the task's
environment it installs these two from the wheelhouse as well, under the task's
pins, so analysis and paper roles can draw figures and tables without a network
(config key `evaluation.extra_packages`; a package missing from the wheelhouse
is logged and left out). It holds no credential and no copy of the task.

The build needs the network once, to download the wheels from the package
index. It then installs them offline in a temporary environment, the pinned
packages first and the plotting packages under the same pins, so a build fails
if the wheelhouse cannot serve them.

A built image was checked by hand:

- The binary, built on the host (glibc), runs in the image (Debian 13, glibc
  2.41, Python 3.12): `polaris-scientist --version` prints the versions and the
  target, and `ldd` resolves every library. A multi-stage build is not needed.
- In a container with `--network none`, `python3 -m venv` followed by
  `pip install --no-index --find-links /opt/wheelhouse numpy==2.4.2
  scipy==1.17.1` installs both, and `import numpy, scipy` works.
- `/opt/polaris-scientist` holds `assets` (prompts, roles, venue notes) and
  `configs/circpack_reduced.toml`. There is no `/task-src`: the task folder is
  mounted at run time and is not part of the image.
- A search of the files added by the build, of `/home`, `/root`, `/etc` and of
  the binary found no access key id and no key text. The only matches in the
  binary are the names `aws_secret_access_key` and `aws_session_token`, which
  the AWS client uses when it reads a credentials file. The image environment
  holds no `AWS_*` value other than `AWS_EC2_METADATA_DISABLED=true`.
- The container user is `agent` (uid 1000). The launcher runs it as the host
  user instead.

That check started no campaign: a campaign needs a model and a credential.

## How a role call ends

The runner returns a report whose `StopCause` says how the call ended, and what
it spent. The order below is the order the rules are applied.

| What happened | Result |
|---|---|
| The campaign was told to stop, or the call's time ran out, and the call was then interrupted (or did not stop within `runner.interrupt_grace_seconds` and was dropped) | `Interrupted` or `Timeout`, named for the request |
| The agent gave a final answer | `Finished` |
| The call's spend limit refused the next request | `CostCap` |
| The session's turn limit ran out | `TurnCap` |
| The provider failed, for example a rejected credential | `ModelError`, with the provider's reason |
| The model has no price, or a finished call reported no tokens | `AgentFailure::Unmeasured`, not a report |
| The folder, a limit or the session could not be set up | `AgentFailure::Setup`, not a report |

A call with a spend limit of zero ends as `CostCap` before it asks the model
anything. A call made after the campaign was told to stop ends as `Interrupted`
and starts no session.

### What a call spends

Each call has a meter of its own with a spend limit equal to the amount the
campaign granted it. Tokens are priced from the built-in price table, so no
price file is read. The cost is rounded up to a whole micro-dollar.

A call that ends early still reports what it spent:

- Out of money: the requests that were allowed are billed exactly. The request
  that would have gone past the limit is not made. A limit that admits one
  request but not the next ends the call after the first.
- Out of turns: every request made is billed.
- A provider failure that bills nothing, such as a rejected credential,
  reports zero.
- Out of time or stopped while a request is in flight: that request leaves no
  token count behind, so it is charged at its estimate, which is the prompt
  plus the full output allowance: the role's `max_output_tokens` when it sets
  one, otherwise `runner.max_output_tokens`. The campaign's total can only be
  higher than the true cost, not lower. The same value is the `max_tokens` the
  call sends with every request, up to 128000: the model provider is
  registered with that ceiling, and a request can lower it but not raise it.
- A reply that reports only one of the two counts, input or output tokens, is
  charged at the same estimate.
- If the model has no price, the call is refused before any request is made.
  If a call finishes but the provider reported no tokens, the call is reported
  as unmeasured and the campaign does not count it as a success.

### Shell commands

A command run by the agent is limited in time and in output.

- It runs for at most `120` seconds by default. The shell tool also takes a
  `timeout_secs` argument, and the shell honours it, so the agent can ask for
  more time (or less) for one command. The limit is therefore a default and
  not a ceiling. What bounds a command in every case is the call's own time
  limit: when it runs out, the call is interrupted and the running command is
  stopped with it.
- A command that runs past its time is killed, and the agent is told that it
  timed out and was killed, and that it can run the command again with a larger
  `timeout_secs` or a narrower command. The time the agent is told is how long
  the command ran in all, which can be a few seconds more than the limit.
- The first 64 KiB of each of standard output and standard error is kept. The
  agent is told which stream was cut.

These limits do not apply to an evaluator run by the campaign itself.

### Credentials

The runner starts the model client once, with the keys from the process
environment. Commands run by the agent start from an empty environment that
holds only `PATH`, `VIRTUAL_ENV` and a few harmless names such as `HOME`.

The tests show:

- A command that lists the environment, prints the key variable by name, or
  echoes it sees no `AWS_*` variable. The key values appear in no model
  request, report, error, log line, transcript or file the runner wrote.
- Once the process has marked itself not dumpable, a command it started as the
  same user cannot read the process's `/proc/<pid>/environ`: the read is
  refused. The command does this before it reads any key.

They do not show, and the runner does not claim, that a key cannot be found
some other way. The AWS client reads its credentials by the SDK's default
chain, which also looks at files in `HOME` and at the instance metadata
service. The launcher gives the container an empty `HOME` and the image turns
the metadata lookup off in the client, so a missing key is an error and not a
network lookup. A command that opens a connection itself is not stopped by
either. The container has network access, which the model and the package index
need.

### Memory over many calls

Every session writes its decisions to an in-memory graph kept by the agent
server for the life of the process, and it is released when the process ends.
It is not trimmed during a run. A test makes 300 scripted calls through one
runner (each with three model requests and two tool calls) and measures the
process:

| | After 50 calls | After 300 calls |
|---|---|---|
| Resident memory (debug build) | 35.2 MiB | 44.6 MiB |
| Open files | 10 | 10 |
| Threads | 7 to 10 | 7 to 10 |

That is about 37 KiB per call, growing in a straight line; a call of one text
reply and no tools adds about 10 KiB. Using one folder for every call grows the
same way. A campaign of a few hundred calls adds some tens of MiB. The figure
for a release build was not measured. Sessions themselves are removed when a
call ends, also when its future is dropped, so nothing else is kept.

## Layout

- `src/runner/` is the session runner: `call.rs` (one call), `report.rs`
  (the mapping above), `server.rs` (the agent server and its shell),
  `transcript.rs` (one transcript file per call).
- `src/commands.rs`, `src/cli.rs`, `src/exit.rs`, `src/signals.rs`,
  `src/hardening.rs` are the command.
- `src/claims_check.rs` is the seam through which `check-claims` reaches the
  offline claims check.
- `launcher/`, `image/`, `configs/` are the container files.

### Advisory author tests

Campaign `program_author`, `variant_author`, and `variant_retrofit` sessions
with edit permission may choose `test_candidate(program_dir, timeout_seconds)`.
The existing CodeAgent tool branch in `polaris_code` handles this choice and
returns the result to its current conversation (`agents/code_agent.rs` and
`agents/code_agent/tool_calls.rs` in that crate). The runner creates one session
for the role call, with no additional session for a test (`agent_config` in
`src/runner/call.rs`).

The tool accepts only the role's output candidate, snapshots it, and permits at
most one test per call. A requested timeout is capped by
`evaluation.timeout_seconds`; the normal evaluator cleanup grace still applies.
Results and logs live under the call's `tests/NNN/` directory, outside the final
candidate. A successful tool result contains advisory metrics and correctness;
comparison authors also receive the existing mechanical CSV checks, so they can
correct instrumentation before official evaluation. A failed or timed-out run
consumes a test and returns an error report. Authors can inspect those artifacts,
repair their candidate, or finish. The graph's
independent evaluation and canonical rubric checks remain authoritative
(`program_eval` in `polaris_scientist/src/phases/investigation/mod.rs` and
`score` in `polaris_scientist/src/phases/ablations/mod.rs`).

Copied scorer artifacts are removed from private candidate copies before testing
and accepting an author output. The host then produces fresh evaluation records;
resuming an already canonical evaluation retains its existing integrity checks
(`src/runner/testing.rs` and `author_check` in
`polaris_scientist/src/phases/ablations/mod.rs`).

Tests reuse the verified task runtime and unchanged evaluator. The child starts
with a cleared environment and inherits Linux Landlock and seccomp restrictions:
no sockets; writes only within its test directory, `/dev/shm` for Python
multiprocessing semaphores, and `/dev/null`. Evaluator and task files stay outside
that writable directory. Mode changes, ptrace, process-memory writes, and
io_uring are refused. Landlock ABI 3 or later is required; unavailable isolation
refuses execution. The existing evaluator supervisor bounds and reaps the child
process tree (`polaris_scientist/src/eval/mod.rs`). These restrictions apply
to advisory tests; they do not change the ordinary evaluator API.

The program-author prompt derives from the original iterative `general.md`
workflow rather than the original host-only `harness_edit.md`. The differences
are explicit: staged paths and writable file cloning replace the upstream
Python clone helper; one host-selected candidate remains in the output folder;
`test_candidate` replaces the evaluator shell wrapper; one advisory test is
available, matching the shipped investigation attempt cap; and author rubric assessments
live in `outputs/author_rubric_status.json`, separate from host verification.
The variant prompt retains the upstream comparison-change instructions and
replaces its evaluation prohibition with the same optional test workflow.
