# Provenance of the article and figures

This trace applies to the published post saved as `article/article.svelte` and its figures
in `article/figures/`. It does not silently certify a later edited article.
Repeated uses of the same assertion in prose, captions, accessibility text and
figures share one entry. Pixel coordinates, CSS sizes, axis ticks, array indexes
and schematic layout constants are presentation code, not experimental numbers.

Run the commands in [README.md](README.md), adding
`--out article-checks.json` to `check_article.py` for the full structured report.
That report checks **193 numeric assertions: 189 match, none differ, three
publication years cannot be established from campaign records, and one
SWE-bench token-ratio assertion belongs to a separate published result**. The detailed
numeric ledger below gives every assertion, its printed and computed values,
and the bundle files and calculation. Narrative assertions are addressed
separately, including claims broader than their supporting measurements.

## Remaining qualifications

- “Same score” means **similar score**: the outside maxima are
  2.630174610525776 and 2.635983084657616, not equal.
- **116/116** counts numeric occurrences within the mechanical extractor's scope.
  It does not prove that every number or scientific claim in the paper is backed.
  Math exemptions can conceal an unsupported replication count; the saved probe
  is `comparison/math-replication-probe.json`.
- Fixed-prompt sizes are fitted initial-text estimates. They are not exact
  constants measured on every request. Average request-size comparisons below
  refer to input exposure per usage-bearing response.
- The campaign records do not establish the publication history, literature
  anecdotes, separate benchmark results, or isolated causal effects of the design.

## Definitions and commands

`reproduce.py` verifies the manifest, then uses the adapted saved-record counter
and action tracer. `measurements.json` contains every raw counter, its input file,
JSON key or line, and the definitions. `check_article.py` compares those counters
with the frozen figure data. `fit_prompts.py` independently recomputes both prompt
regressions from their saved observations.

A “model call” in this article means a role invocation/session, not one provider
request: the reported campaign has 33 role invocations and 339 metered requests,
of which 334 have usage-bearing responses. The paper writer has 69 metered
requests and 67 usage-bearing responses. Calling all of these “turns” without
that distinction is ambiguous. Original native turns use another convention.

Money from token usage uses the saved price test in
`implementation/crates/polaris_scientist_run/tests/prices.rs`: fresh input $2,
cache creation $2.50, cache reads $0.20 and output $10 per million tokens.
Polaris recorded-token cost is $9.0091552; original recorded-token cost is
$9.0894467. These are the token-priced values used in the article.

Original wall time needs a convention because no final `pipeline_end` was saved.
The article's 75 minutes sums the first completed segment and the interval from
the paper launch to the failed finalization gate, excluding the 545.477-second
wait between launches. This is about 4514.6 seconds; process shutdown is unknown.
The original paper bar uses that second interval, about 2327.5 seconds. Polaris
has a complete 3438.819-second event span, or 57 minutes rounded. Task setup and
final evaluation are excluded from the phase bars. Those excluded phases take
under a minute on each side, and contain no recorded model calls.

The average writer input ratio of 1.6 is
`(7879122 / 67) / (2055344 / 28)`, rounded to one decimal. Input includes fresh,
cache-created and cache-read tokens. “Larger” refers to average input exposure,
not output length or maximum context. The 15,400/2,500 fixed-text numbers are
rounded fitted intercepts, 15,421 and 2,521. The former fit uses 18 distinct points
with 2.04% worst relative residual; the latter uses four selected calls and an
assumed tool-result ratio, with 8.27% worst residual. “Every request carries”
should therefore be qualified as an estimate of the initial fixed text.

## Narrative and qualitative claims

The rows follow the article's prose and caption order. “Qualified” means the
records support a narrower statement; “cannot trace” means the required evidence
is absent from this bundle. “Outside records” identifies an assertion about
a separate published result, product or release rather than a discrepancy with
these campaigns. Future plans and invitations are statements of intent, not
measurements.

| Assertion and location | Bundle evidence and reproducible inspection | Result |
|---|---|---|
| Original takes a task through ideas, programs, ablations, analysis and paper; its stages use a coding agent | `campaigns/original/pipeline_events.jsonl`, `metrics/cost_events.jsonl`, scientific conversations; `reproduce.py` phase/role attribution | Supported for this task; general product claims need the original documentation. |
| Third-generation name, first/second/third architectures, years 2024/2025/2026, and prior paper designs in Topologies | `article/figures/Topologies.svelte` records the assertion, but the cited research papers are not campaign records | Cannot trace historical claims from this bundle; publication years appear in the numeric ledger. |
| Polaris uses nodes/edges for phase order and checks; a role invocation contains model requests | `implementation/crates/polaris_scientist/src/phases/{investigation,ablations,paper}/`, `campaigns/polaris-reported/work/`, event order; `reproduce.py` | Supported for the saved implementation and run. The model still chooses tools inside role sessions. |
| Each role invocation runs as a separate Polaris Code session | `campaigns/polaris-reported/pipeline_events.jsonl#pipeline_start.fields.execution.runner`, per-invocation `work/**/transcript.jsonl`, and runner source under `implementation/` | Supported for the session runner used by this campaign. A role session contains multiple provider requests. |
| Polaris Code matched Claude Code on SWE-bench Verified using about one sixth of the tokens | `article/article.svelte` links to `polaris-code-swe-bench-tokens`; those benchmark runs are not in this bundle | Outside records: a separate published comparison. The token ratio is inventoried below without being classified as a mismatch or recomputed from circle-packing data. |
| Same task and same model | `campaigns/*/task/task.md`, `task/evaluate.py`, cost ledgers' `model`, both task snapshots; `reproduce.py` | Supported: circle packing in a unit square; all recorded campaign model IDs resolve to Sonnet 5.5. |
| Similar result, same-length compiled papers, same overall review score and higher clarity/soundness/presentation sub-scores | Outside `rescores/*/best*/metrics.json` and `correct.json`; paper LaTeX logs and PDFs; `reviews/*/summary.json`; `reproduce.py` checks reviewed PDF hashes | Both PDFs have 13 pages and overall review 2 / Reject. Polaris scores higher on the three named sub-scores; exact packing scores are similar, not equal. |
| Task setup and scoring use no model; phase and call graph captions | Event/ledger phase attribution and official evaluation manifests; `reproduce.py` | No recorded model calls in task drafting or final evaluation. Official scoring is a host-code operation. |
| Full graph call order, roles, request counts and durations | `campaigns/polaris-reported/pipeline_events.jsonl`; `check_article.py`, rows below | All 33 request counts and all displayed durations match nearest-second event timing. |
| First figure plan rejected for unsupported type, second accepted | `campaigns/polaris-reported/work/paper/strategy*/`, their transcripts and validation outputs; event order | Supported; the two strategy invocations precede writing. |
| Three failed evidence audits, three repairs, fourth audit passes; bounded to ten repairs | `campaigns/polaris-reported/paper/rounds/`, `paper_manifest.json`, resolved paper settings, role ledgers; `reproduce.py` and `check_article.py` | Supported. The original records eleven auditor invocations and ten repairs, ending with failed finalization. |
| Original paper reports agents altering time limits and voting to change evaluation | No cited-paper text or primary observation in this campaign bundle | Cannot trace the literature anecdote. It is not an observed event in these three campaigns. |
| Original checks artifacts after stages | `campaigns/original/paper/validation_report.json`, evidence audits, rubric and evaluation records, `pipeline_events.jsonl` | Supported for the recorded checks. |
| Read-only inputs, empty outputs, limited role budgets, independently scored program, keep-best behavior in StepAnatomy | Resolved role settings; `implementation/crates/polaris_scientist/src/phases/investigation/`; evaluation manifests; `comparison/source-inspections.json` frozen-input inspection | Supported as implemented behavior. Saved filesystem modes alone are not a general isolation proof. |
| Each time the model writes a program, it can test once | `implementation/crates/polaris_scientist_run/src/runner/testing.rs`; `work/**/tests/001/report.json`; `comparison/independent-checks.json` raw test counts | Supported: one test per author invocation. The reported campaign has six advisory tests. |
| Official score comes from separate frozen evaluation rather than the advisory test | Investigation/ablation host code under `implementation/`; `evaluation_manifest.json` and `rubric_status.json` next to candidates; scorer fingerprints in independent checks | Supported for saved official evaluations. This does not certify arbitrary hostile shell execution. |
| Twenty roles; twelve without shells, eight with named lists; program counts and read-only auditor in Permissions | `campaigns/polaris-reported/roles.resolved.toml`, role names in `pipeline_events.jsonl`; `reproduce.py` and `check_article.py` | Configured policy matches every displayed program count. Auditor is configured read-only. A known parser bypass limits the enforcement claim. |
| Original gives every role unrestricted shell | Scientific transcripts' tool inventory and `comparison/instructions/role-table.json` | Supported as the compared coding-agent configuration; not an assertion that every role used Bash. |
| Every printed number is found before paper passes; 116/116 versus 62/143 in Grounding | `checks/*/claims-v4.json#mechanical_check`; saved claims implementation; `comparison/math-replication-probe.json`; `reproduce.py` | Counts match. Universal wording exceeds extractor scope and known exemptions. Matching values do not establish correct scientific interpretation. |
| 244 attempted commands, 29 refusals, writer 10/73; mostly benign; every refusal costs a turn | Raw `work/**/transcript.jsonl` tool start/end pairs; `comparison/instructions/ours-denied-commands.json`; `check_article.py` | Exact counts match. Named examples occur in saved denials. A denial returns tool feedback; it need not cause a separate provider request by itself. |
| The named-command list could be tuned to refuse fewer benign commands | Same denial records and resolved patterns | Interpretation/design proposal, not a measured improvement. |
| No adversarial cheating test in this campaign; an ImpossibleBench post is coming soon | Campaign task/ledgers establish circle packing; no ImpossibleBench experiment is bundled | Campaign limitation supported. The announced post and its proposed comparisons are outside these records, with no numerical result asserted here. |
| Every recorded Polaris role invocation has a conversation; original coverage and missing ablation sessions in Records | Ledger-to-stream usage matching in `reproduce.py`; raw conversations and event IDs | Supported: Polaris 33/33, original 33/56, with 22 absent and one overwritten. Paper-phase coverage is 28/32. All seventeen original ablation primary streams are absent. |
| Two of three earlier ablations were unchanged; subsequent check rejects unchanged variants | `comparison/earlier-variants/`: byte comparison of each `program_1/src/program.py` to the control; ablation gate implementation; `check_article.py` | Supported: two of three interventions are byte-identical. This small source excerpt is retained for the anecdote, not counted as another reported campaign. |
| Reported campaign's edited ablations pass the source-difference check | `campaigns/polaris-reported/ablations/variants/` source, verification and rubric records; `comparison/independent-checks.json#polaris-reported.variants` | Supported: three edited ablations differ; the control and reference baseline are not edited interventions. |
| Heredoc bypass existed in the reported build and was found later | `implementation/crates/polaris_code/src/tools/command_check.rs`, `comparison/command-probes.json` | Supported: saved probes demonstrate acceptance of an unlisted command through `<<-`. No recorded campaign command uses that form. |
| A fix exists but was absent from the build | Saved command checker and `implementation/command-check-fix.diff` | Supported: the later patch refuses ambiguous heredoc forms and is distinct from the saved checker used by these campaigns. |
| One run per side proves neither speed nor cost gain; unequal search budgets and repair work favor Polaris | Winner sources (`TIME_BUDGET`), paper role counts, timing and cost measurements | Supported limitation. 110 versus 200 seconds and four versus eleven audits match. The comparison does not establish a repeatable speed advantage. |
| 57 versus 75 minutes; per-phase timing bars and excluded non-model phases | Event timeline calculation described above and `check_article.py` | Matches the documented original active-time convention. Original process shutdown time is unknown. |
| $9.01 versus $9.09 priced usage | Campaign ledgers, price source, `comparison/tokens.json`; `reproduce.py` | Supported after rounding the costs computed from recorded token usage. |
| About 15,400 versus 2,500 fixed tokens on every request | `comparison/instructions/orig-context.json`, `ours-first-request.json`; `fit_prompts.py` | Fitted intercepts match after rounding. These are initial fixed-text estimates with modeling assumptions, not exact per-request constants. |
| Writer sends 67 versus 28 requests, each 1.6 times larger on average | Writer raw conversations and their ledger input totals; `comparison/tokens.json`; `check_article.py` | Matches usage-bearing response counts and average total input exposure. Polaris meter attempts are 69, not 67. |
| Ours costs slightly less mostly because of fewer audits and repairs | Per-role costs and token counters in `comparison/tokens.json`; `reproduce.py` | The token-priced total and reduced audit/repair work are supported. “Mostly because” is an interpretation of this pair; differing roles, prompts, outputs, advisory tests and workloads prevent isolated causal attribution. |
| Each node reports tokens and gives a place to optimize | Role ledgers plus request token records in transcripts; `reproduce.py` | Role totals are recorded; exact per-request cache-tier splits are not available for every Polaris response. |
| Graph can enforce read/run/scoring/recording rules | Role configuration, testing code, official evaluation manifests and saved security probes | Supported as concrete mechanisms with the disclosed parser defect; not a universal security guarantee. |
| The lab studies safe agentic systems, including behavior under pressure, long conversations, internal changes and steering | `article/article.svelte` links to `stress-testing-ai-agent-emotional-boundaries`, `looking-inside-an-agent-as-its-boundaries-erode` and `how-hard-to-steer-an-agents-boundaries` | Outside records: these earlier posts describe separate research. No result from them is inferred from the circle-packing campaigns. |
| Automated research can reward changes to evaluation; checking AI research behavior is a safety requirement | The safety section's argument; the original paper's time-limit anecdote is a literature citation rather than a saved campaign observation | A motivation and interpretation, not a new measured result. The cited historical behavior is outside these records. |
| Before a step, readers can inspect its read/run policy; afterwards they can inspect the full conversation and trace paper numbers | Resolved role settings, raw conversation files and `checks/polaris-reported/claims-v4.json`; `reproduce.py` and `check_article.py` | The configured policies, 33 saved role conversations and 116 checked numeric occurrences are supported. The same disclosed parser defect and extractor-scope qualifications apply to this restatement in the safety section. |
| Radiant Platform records runs so people and its Meta Agent can investigate their outcomes | `article/article.svelte` links to `introducing-radiant-platform`; no Platform run or product implementation is bundled | Outside records: a product description and connection to the lab's earlier post, not a claim about measurements in these campaigns. |
| One task, one model, one compared completed campaign per side | Task snapshots, model IDs and cost/event ledgers | Supported for the compared completed campaigns. The original resumed once. |
| Future equal-budget, multiple-run and editable-evaluator experiments; graph visibility and testability | Statements of intent and interpretation | Not presented as completed measurements. |
| Code is open source in the linked repository; records and recomputation scripts are attached to its first release | `article/article.svelte` links to `https://github.com/RadiantAILabs/auditable-scientist`; this bundle supplies the saved records and the repository's `records/` supplies the scripts | Outside the campaign measurements: repository licensing and release attachment are publication metadata. The archive and scripts can be checked using README.md; this offline trace does not verify live hosting or upload the release. |
| Original code published under Apache-2.0 | `implementation/original/LICENSE` | Licence text supports Apache-2.0. The publication event and repository history are not established by a saved licence file alone. |

## Numeric assertion ledger

The following table is generated from `check_article.py`. Paths are relative to
the extracted archive; `campaign.ts`, `ResultsTable.svelte` and the other figure
names refer to `article/figures/`. `measurements.json` is the output of
`reproduce.py`, not an extra source of evidence.

<!-- numeric-ledger -->

| Assertion | Printed | Records / computation | Status | Figure or prose | Bundle evidence |
|---|---|---|---|---|---|
| call 1: ordinal | 1 | 1 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 1: wall time | 0:36 | 0:36 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 1: requests | 7 | 7 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 2: ordinal | 2 | 2 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 2: wall time | 0:35 | 0:35 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 2: requests | 6 | 6 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 3: ordinal | 3 | 3 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 3: wall time | 2:05 | 2:05 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 3: requests | 8 | 8 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 4: ordinal | 4 | 4 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 4: wall time | 0:56 | 0:56 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 4: requests | 8 | 8 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 5: ordinal | 5 | 5 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 5: wall time | 2:06 | 2:06 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 5: requests | 9 | 9 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 6: ordinal | 6 | 6 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 6: wall time | 2:44 | 2:44 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 6: requests | 11 | 11 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 7: ordinal | 7 | 7 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 7: wall time | 0:30 | 0:30 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 7: requests | 4 | 4 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 8: ordinal | 8 | 8 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 8: wall time | 0:17 | 0:17 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 8: requests | 3 | 3 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 9: ordinal | 9 | 9 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 9: wall time | 2:08 | 2:08 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 9: requests | 8 | 8 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 10: ordinal | 10 | 10 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 10: wall time | 0:29 | 0:29 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 10: requests | 9 | 9 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 11: ordinal | 11 | 11 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 11: wall time | 2:02 | 2:02 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 11: requests | 7 | 7 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 12: ordinal | 12 | 12 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 12: wall time | 0:27 | 0:27 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 12: requests | 8 | 8 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 13: ordinal | 13 | 13 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 13: wall time | 2:06 | 2:06 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 13: requests | 7 | 7 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 14: ordinal | 14 | 14 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 14: wall time | 0:27 | 0:27 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 14: requests | 6 | 6 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 15: ordinal | 15 | 15 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 15: wall time | 0:10 | 0:10 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 15: requests | 4 | 4 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 16: ordinal | 16 | 16 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 16: wall time | 1:07 | 1:07 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 16: requests | 7 | 7 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 17: ordinal | 17 | 17 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 17: wall time | 0:15 | 0:15 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 17: requests | 4 | 4 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 18: ordinal | 18 | 18 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 18: wall time | 1:58 | 1:58 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 18: requests | 15 | 15 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 19: ordinal | 19 | 19 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 19: wall time | 0:27 | 0:27 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 19: requests | 4 | 4 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 20: ordinal | 20 | 20 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 20: wall time | 1:11 | 1:11 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 20: requests | 6 | 6 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 21: ordinal | 21 | 21 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 21: wall time | 1:42 | 1:42 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 21: requests | 7 | 7 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 22: ordinal | 22 | 22 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 22: wall time | 0:09 | 0:09 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 22: requests | 3 | 3 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 23: ordinal | 23 | 23 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 23: wall time | 11:42 | 11:42 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 23: requests | 69 | 69 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 24: ordinal | 24 | 24 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 24: wall time | 0:29 | 0:29 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 24: requests | 5 | 5 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 25: ordinal | 25 | 25 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 25: wall time | 0:21 | 0:21 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 25: requests | 4 | 4 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 26: ordinal | 26 | 26 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 26: wall time | 1:52 | 1:52 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 26: requests | 20 | 20 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 27: ordinal | 27 | 27 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 27: wall time | 0:55 | 0:55 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 27: requests | 12 | 12 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 28: ordinal | 28 | 28 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 28: wall time | 1:42 | 1:42 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 28: requests | 22 | 22 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 29: ordinal | 29 | 29 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 29: wall time | 0:57 | 0:57 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 29: requests | 9 | 9 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 30: ordinal | 30 | 30 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 30: wall time | 0:17 | 0:17 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 30: requests | 7 | 7 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 31: ordinal | 31 | 31 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 31: wall time | 1:07 | 1:07 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 31: requests | 11 | 11 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 32: ordinal | 32 | 32 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 32: wall time | 0:44 | 0:44 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 32: requests | 15 | 15 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| call 33: ordinal | 33 | 33 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end order |
| call 33: wall time | 1:10 | 1:10 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps |
| call 33: requests | 14 | 14 | match | campaign.ts | campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns |
| campaign call count | 33 | 33 | match | campaign.ts; CampaignGraph.svelte; article.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl |
| polaris-reported: best outside score | 2.630 | 2.630 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl; checks/polaris-reported/claims-v4.json; reviews/polaris-reported/summary.json; rescores/polaris-reported/ |
| polaris-reported: overall reviewer score | 2 | 2 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl; checks/polaris-reported/claims-v4.json; reviews/polaris-reported/summary.json; rescores/polaris-reported/ |
| polaris-reported: printed numbers | 116 | 116 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl; checks/polaris-reported/claims-v4.json; reviews/polaris-reported/summary.json; rescores/polaris-reported/ |
| polaris-reported: backed numbers | 116 | 116 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl; checks/polaris-reported/claims-v4.json; reviews/polaris-reported/summary.json; rescores/polaris-reported/ |
| polaris-reported: ledger calls | 33 | 33 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl; checks/polaris-reported/claims-v4.json; reviews/polaris-reported/summary.json; rescores/polaris-reported/ |
| polaris-reported: full matching conversations | 33 | 33 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl; checks/polaris-reported/claims-v4.json; reviews/polaris-reported/summary.json; rescores/polaris-reported/ |
| polaris-reported: token dollars | 9.01 | 9.01 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl; checks/polaris-reported/claims-v4.json; reviews/polaris-reported/summary.json; rescores/polaris-reported/ |
| original: best outside score | 2.636 | 2.636 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/original/metrics/cost_events.jsonl; checks/original/claims-v4.json; reviews/original/summary.json; rescores/original/ |
| original: overall reviewer score | 2 | 2 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/original/metrics/cost_events.jsonl; checks/original/claims-v4.json; reviews/original/summary.json; rescores/original/ |
| original: printed numbers | 143 | 143 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/original/metrics/cost_events.jsonl; checks/original/claims-v4.json; reviews/original/summary.json; rescores/original/ |
| original: backed numbers | 62 | 62 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/original/metrics/cost_events.jsonl; checks/original/claims-v4.json; reviews/original/summary.json; rescores/original/ |
| original: ledger calls | 56 | 56 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/original/metrics/cost_events.jsonl; checks/original/claims-v4.json; reviews/original/summary.json; rescores/original/ |
| original: full matching conversations | 33 | 33 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/original/metrics/cost_events.jsonl; checks/original/claims-v4.json; reviews/original/summary.json; rescores/original/ |
| original: token dollars | 9.09 | 9.09 | match | ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte | campaigns/original/metrics/cost_events.jsonl; checks/original/claims-v4.json; reviews/original/summary.json; rescores/original/ |
| polaris-reported investigation: calls | 5 | 5 | match | Records.svelte; Topologies.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl; pipeline_events.jsonl |
| polaris-reported investigation: conversations | 5 | 5 | match | Records.svelte | campaigns/polaris-reported/ + calls[].stream in measurements.json |
| original investigation: calls | 4 | 4 | match | Records.svelte; Topologies.svelte | campaigns/original/metrics/cost_events.jsonl; pipeline_events.jsonl |
| original investigation: conversations | 4 | 4 | match | Records.svelte | campaigns/original/ + calls[].stream in measurements.json |
| polaris-reported ablations: calls | 10 | 10 | match | Records.svelte; Topologies.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl; pipeline_events.jsonl |
| polaris-reported ablations: conversations | 10 | 10 | match | Records.svelte | campaigns/polaris-reported/ + calls[].stream in measurements.json |
| original ablations: calls | 17 | 17 | match | Records.svelte; Topologies.svelte | campaigns/original/metrics/cost_events.jsonl; pipeline_events.jsonl |
| original ablations: conversations | 0 | 0 | match | Records.svelte | campaigns/original/ + calls[].stream in measurements.json |
| polaris-reported posthoc: calls | 3 | 3 | match | Records.svelte; Topologies.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl; pipeline_events.jsonl |
| polaris-reported posthoc: conversations | 3 | 3 | match | Records.svelte | campaigns/polaris-reported/ + calls[].stream in measurements.json |
| original posthoc: calls | 3 | 3 | match | Records.svelte; Topologies.svelte | campaigns/original/metrics/cost_events.jsonl; pipeline_events.jsonl |
| original posthoc: conversations | 1 | 1 | match | Records.svelte | campaigns/original/ + calls[].stream in measurements.json |
| polaris-reported paper: calls | 15 | 15 | match | Records.svelte; Topologies.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl; pipeline_events.jsonl |
| polaris-reported paper: conversations | 15 | 15 | match | Records.svelte | campaigns/polaris-reported/ + calls[].stream in measurements.json |
| original paper: calls | 32 | 32 | match | Records.svelte; Topologies.svelte | campaigns/original/metrics/cost_events.jsonl; pipeline_events.jsonl |
| original paper: conversations | 28 | 28 | match | Records.svelte | campaigns/original/ + calls[].stream in measurements.json |
| original missing or unusable conversations | 23 | 23 | match | article.svelte#Records caption | campaigns/original/metrics/cost_events.jsonl + matching stream usage |
| original never-saved conversations | 22 | 22 | match | article.svelte#Records caption | campaigns/original/metrics/cost_events.jsonl + stream inventory |
| original overwritten conversations | 1 | 1 | match | article.svelte#Records caption | campaigns/original/paper/ + mismatched final stream usage |
| named shell programs: ideation | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.ideation.shell |
| named shell programs: program author | 12 | 12 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.program_author.shell |
| named shell programs: variant retrofit | 15 | 15 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.variant_retrofit.shell |
| named shell programs: variant proposer | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.variant_propose.shell |
| named shell programs: variant selector | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.variant_select.shell |
| named shell programs: variant author | 12 | 12 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.variant_author.shell |
| named shell programs: variant verifier | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.variant_verifier.shell |
| named shell programs: variant summariser | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.variant_summarizer.shell |
| named shell programs: post-hoc proposer | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.posthoc_proposer.shell |
| named shell programs: post-hoc selector | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.posthoc_selector.shell |
| named shell programs: post-hoc analyst | 15 | 15 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.posthoc_analyst.shell |
| named shell programs: reference planner | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.reference_planner.shell |
| named shell programs: paper strategist | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.paper_strategist.shell |
| named shell programs: post-hoc ranker | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.posthoc_ranker.shell |
| named shell programs: paper writer | 18 | 18 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.paper_writer.shell |
| named shell programs: citation gap analyst | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.citation_gap_analyst.shell |
| named shell programs: reference planner, after the draft | 0 | 0 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.reference_planner_postdraft.shell |
| named shell programs: paper enricher | 18 | 18 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.paper_enricher.shell |
| named shell programs: paper repairer | 18 | 18 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.paper_repairer.shell |
| named shell programs: evidence auditor | 13 | 13 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml#roles.evidence_auditor.shell |
| roles used | 20 | 20 | match | Permissions.svelte; article.svelte | campaigns/polaris-reported/pipeline_events.jsonl#agent_start.fields.role |
| shell attempts | 244 | 244 | match | article.svelte | campaigns/polaris-reported/work/**/transcript.jsonl#tool run_command |
| shell refusals | 29 | 29 | match | article.svelte | campaigns/polaris-reported/work/**/transcript.jsonl#tool run_command |
| writer shell attempts | 73 | 73 | match | article.svelte | campaigns/polaris-reported/work/paper/draft/001/transcript.jsonl |
| writer shell refusals | 10 | 10 | match | article.svelte | campaigns/polaris-reported/work/paper/draft/001/transcript.jsonl |
| polaris writer usage responses | 67 | 67 | match | article.svelte | campaigns/polaris-reported/work/paper/draft/001/transcript.jsonl |
| reference writer usage responses | 28 | 28 | match | article.svelte | campaigns/original/paper/paper_agent_claude_20261105_210204.log |
| writer average input-size ratio | 1.6 | 1.6 | match | article.svelte | comparison/tokens.json#runs.*.calls[paper_writer].context |
| polaris-reported PDF pages | 13 | 13 | match | ResultsTable.svelte | campaigns/polaris-reported/paper/ + saved LaTeX log |
| original PDF pages | 13 | 13 | match | ResultsTable.svelte | campaigns/original/paper/ + saved LaTeX log |
| Polaris investigation minutes | 8.8 | 8.8 | match | TimeByPhase.svelte | campaigns/polaris-reported/pipeline_events.jsonl#phase_end.elapsed_seconds |
| original investigation minutes | 10.4 | 10.4 | match | TimeByPhase.svelte | campaigns/original/pipeline_events.jsonl#phase_end.elapsed |
| Polaris ablations minutes | 18.3 | 18.3 | match | TimeByPhase.svelte | campaigns/polaris-reported/pipeline_events.jsonl#phase_end.elapsed_seconds |
| original ablations minutes | 24.5 | 24.5 | match | TimeByPhase.svelte | campaigns/original/pipeline_events.jsonl#phase_end.elapsed |
| Polaris posthoc minutes | 3.3 | 3.3 | match | TimeByPhase.svelte | campaigns/polaris-reported/pipeline_events.jsonl#phase_end.elapsed_seconds |
| original posthoc minutes | 1.5 | 1.5 | match | TimeByPhase.svelte | campaigns/original/pipeline_events.jsonl#phase_end.elapsed |
| Polaris paper minutes | 26.6 | 26.6 | match | TimeByPhase.svelte | campaigns/polaris-reported/pipeline_events.jsonl#phase_end.elapsed_seconds |
| Polaris whole-run minutes | 57 | 57 | match | ResultsTable.svelte; TimeByPhase.svelte; article.svelte | campaigns/polaris-reported/pipeline_events.jsonl#pipeline_start/end |
| original paper minutes | 38.8 | 38.8 | match | TimeByPhase.svelte | campaigns/original/pipeline_events.jsonl#second start to failed finalization |
| original active minutes | 75 | 75 | match | ResultsTable.svelte; TimeByPhase.svelte; article.svelte | campaigns/original/pipeline_events.jsonl#closed first segment plus second start to finalization; shutdown unknown |
| repair round limit | 10 | 10 | match | article.svelte | campaigns/polaris-reported/config.resolved.toml#paper.max_repair_rounds |
| Polaris audit calls | 4 | 4 | match | article.svelte; ResultsTable.svelte; TimeByPhase.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl#stage |
| original audit calls | 11 | 11 | match | ResultsTable.svelte; TimeByPhase.svelte | campaigns/original/metrics/cost_events.jsonl#stage |
| polaris-reported winner search budget | 110 | 110.0 | match | TimeByPhase.svelte | campaigns/polaris-reported/investigation/best_program_manifest.json -> src/program.py#TIME_BUDGET |
| original winner search budget | 200 | 200.0 | match | TimeByPhase.svelte | campaigns/original/investigation/best_program_manifest.json -> src/program.py#TIME_BUDGET |
| earlier ablation intervention count | 3 | 3 | match | article.svelte | comparison/earlier-variants/ablation_*/task_bundle/program_1/src/program.py |
| earlier unchanged interventions | 2 | 2 | match | article.svelte | comparison/earlier-variants/ + byte comparison to control_best |
| polaris-reported distinct models | 1 | 1 | match | article.svelte | campaigns/polaris-reported/metrics/cost_events.jsonl#model |
| original distinct models | 1 | 1 | match | article.svelte | campaigns/original/metrics/cost_events.jsonl#model |
| Polaris roles without shell | 12 | 12 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml + pipeline_events.jsonl#roles used |
| Polaris roles with shell | 8 | 8 | match | Permissions.svelte | campaigns/polaris-reported/roles.resolved.toml + pipeline_events.jsonl#roles used |
| polaris-reported worker count | 1 | 1 | match | ResultsTable.svelte | campaigns/polaris-reported/pipeline_events.jsonl#pipeline_start execution workers |
| original worker count | 1 | 1 | match | ResultsTable.svelte | campaigns/original/pipeline_events.jsonl#pipeline_start execution workers |
| advisory tests per author invocation | 1 | 1 | match | article.svelte#StepAnatomy caption | implementation/crates/polaris_scientist_run/src/runner/testing.rs#MAX_TESTS |
| Polaris estimated fixed prompt tokens | 2500 | 2500 | match | TimeByPhase.svelte; article.svelte | comparison/instructions/ours-token-fit.json; fit_prompts.py |
| original estimated fixed prompt tokens | 15400 | 15400 | match | TimeByPhase.svelte; article.svelte | comparison/instructions/orig-token-fit.txt; fit_prompts.py |
| first system publication year | 2024 | None | cannot trace | Topologies.svelte | Publication history requires the cited papers; campaign records are not publication records. |
| second system publication year | 2025 | None | cannot trace | Topologies.svelte | Publication history requires the cited papers; campaign records are not publication records. |
| third system publication year | 2026 | None | cannot trace | Topologies.svelte | Publication history requires the cited papers; campaign records are not publication records. |
| SWE-bench Verified token ratio in the separately published comparison | about one sixth | None | outside records | article.svelte#Polaris Code introduction | The linked polaris-code-swe-bench-tokens post reports a separate benchmark; no SWE-bench campaign is included here. |
