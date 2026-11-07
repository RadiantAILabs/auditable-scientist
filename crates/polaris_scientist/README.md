# Polaris Scientist

Research campaigns composed as Polaris graphs. `campaign` wires phase graphs;
`config`, `roles`, `prompts` and `records` own configuration and durable evidence.
The runtime crate supplies an interchangeable agent session runner.

Run the offline checks with `cargo +1.97.0 test -p polaris_scientist`.

Phase systems read `Res<ScientistConfig>` and `Res<RunHandle>`; the campaign
installs both as globals. The configuration supports `value`, `get<T>`, `u64`,
`bool`, `string`, `path`, `micros` and `fraction` dotted-key accessors.

Compose phase graphs using these `polaris_cosmos::flow` interfaces:

- `agent(id, role) -> Graph` reads `Out<AgentCall>` and emits `AgentOutcome`.
  `AgentCall.step` is the durable identity. Calls stage into
  `work/<step>/001`, `002`, and so on. `AgentRequest.step` retains the durable
  identity, while `AgentRequest.workspace` identifies that individual invocation.
- `lease_loop(name, width, build_lane) -> Graph` calls `build_lane(worker)`
  at construction. Each body reads `Out<LaneLease>` containing `worker` and
  `lease: Option<Lease>` with global and worker-local one-based indices.
  `Verdict::Fail` ends that lane after acknowledging its lease; other lanes finish.
- `worklist(name, body) -> Graph` supplies `Out<CurrentItem>` wrapping
  `Option<WorkItem>`. The body emits `Verdict::Pass` or `Verdict::Fail(feedback)`.
  Each result is recorded before the next item. Materialization can call
  `Worklist::replace(items)` after proposals exist; persisted matching keys keep
  their completed or failed result.
- `repair_loop(name, counter, check, repair) -> Graph` runs a check emitting
  `Verdict`, consumes the shared named counter before each repair, then checks
  again. Per-item initialization may call `RepairCounters::reset(name, limit)`;
  memoize that initialization so a restart preserves consumed units.
- `memo::<T>(key, body) -> Graph` restores a completed typed output or runs
  the body and records it. Every iteration needs its own durable key.
- `rounds(name, count, body, barrier) -> Graph` repeats the body followed by
  an explicit barrier. `counter`, `budget`, and `events` are small graph builders
  over their corresponding resource contracts.
- `phase(name, policy, body) -> Graph` adopts the body's `PhaseStatus`, including
  `Degraded(reason)`. Checks can also publish status through `PhaseControl`.

`StageSpec.vars` overrides standard prompt variables, including `TASK_DIR`.
Its default is the private workspace's `inputs/task`. Skills are directories
relative to the prompt root, such as `research/skills/edit-program`; their
copies are staged read-only under `skills/<name>`. Markdown skills are rendered
with the main prompt; unused-variable checks cover their combined placeholders.

The campaign runs on its own small server with persistence and session plugins;
the injected `TaskRunner` owns the agent runtime. `run.open`, the wrapped phase
bodies, the winner branch and `run.close` are graph nodes. Completed phases and
steps restore their durable outputs on resume. Interrupted steps repeat in a
new numbered folder, and their old folders move to `work/_abandoned/`.
A dropped call is settled at its entire reserved ceiling before new calls begin.
Reset archives affected phase artifacts and completion records; settled money
remains spent.

The `core` target checks strict configuration, merged roles and graph role
coverage. The `e2e` target uses small replacement phase graphs with an injected
scripted runner. It checks phase order, the two winner branches, failure
policies, graceful interruption, abrupt cancellation, resume and reset. Its
two-lane test completes both shared leases and settles both calls before the
join. The whole `e2e` target runs in about a second. No model, credential,
container or network is involved.

```sh
cargo +1.97.0 test -p polaris_scientist --test core --test e2e
```

`tests/scenarios/campaign/scenario.toml` combines the individual phase scenarios
into one fixture campaign. Installed runtimes can set `POLARIS_SCIENTIST_ASSETS`
to the shipped assets directory.

`memo_current::<T>(body)` reads an upstream `Out<StepKey>` when an iteration's
key is only known at runtime. It records and restores each item's typed output
without changing the graph shape.

Phase deadlines are cooperative. No new role call starts after expiry, and a
role's timeout is reduced to the remaining phase time. A deterministic step
already running finishes under its own timeout; phase completion then applies
its failure policy. This preserves the graph's continuation and `run.close`:
in `polaris_graph` 0.8.1 a scope timeout propagates out of the entire graph,
while system error handlers target only fallible systems. The campaign wall
guard outside the graph supplies the hard outer bound. Event sinks emit heartbeats every sixty seconds during a phase and
cancel their timers at phase completion or shutdown.

Investigation, ablation and post-hoc execution timeout settings seed their
corresponding roles before explicit `roles.<key>.timeout_seconds` overrides.
The resolved role book records the timeout each agent request receives.
