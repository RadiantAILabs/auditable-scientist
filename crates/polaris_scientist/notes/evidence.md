# Evidence: the paper phase, the review phase and the claims check

This note records what was measured about the paper and review phases, how
they differ from the original Agentic Scientist (AI-Scientist-v3), and what is
not built. "The original" means that system.

Where things are:

| Part | Path |
| --- | --- |
| Paper phase | `src/phases/paper/` |
| Review phase | `src/phases/review/mod.rs` |
| Outside claims check | `src/claims/` |
| TeX build and TeX text helpers | `src/latex/` |
| Reference retrieval | `src/phases/paper/refs.rs` |
| Prompts, skills, licence | `assets/prompts/paper/` |
| Roles | `assets/roles/evidence.toml` |
| Venue template | `assets/venues/iclr/` |
| Tests, scenarios, stand-ins | `tests/evidence/`, `tests/scenarios/{paper,review}`, `tests/fixtures/paper/` |

The tests run with no model, no credentials, no container and no outside
network. TeX is a stand-in program (`tests/fixtures/paper/fake_pdflatex.sh`),
and the reference services are a stand-in server on `127.0.0.1`.

## Measurements

### Reference services

One request to each service on 2026-11-06, with no key, gave:

| Service | Request | Answer |
| --- | --- | --- |
| Semantic Scholar | `GET /graph/v1/paper/search?query=circle+packing&limit=1` | 429 (too many requests) |
| OpenAlex | `GET /works?search=circle+packing&per-page=1` | 200 |

Without a key Semantic Scholar refuses at once on a shared address. Retrieval
therefore waits and asks again (up to five tries per request, using the
`Retry-After` header when there is one), gives the service up after three
failed requests in a row, and goes on to OpenAlex. A run that sets
`S2_API_KEY` sends it to Semantic Scholar. `OPENALEX_EMAIL` is sent to OpenAlex
as a contact address. Neither is needed.

`POLARIS_SCIENTIST_OFFLINE=1` makes a run contact nobody. The provenance file
then says so under `attempts`, the reference counts stay short, and the
evidence audit reports it. `POLARIS_SCIENTIST_S2_URL` and
`POLARIS_SCIENTIST_OPENALEX_URL` replace the service addresses, which is how
an offline run can use a local stand-in.

A third source, the `scholarly` package of the original, is not built (see
below).

### TeX

The tests need no TeX installation. They run the TeX build and the page count
against a stand-in program, which prints the log lines the build reads
(`Output written on …` and the `PSPROBE` lines) in the format the build
expects. The stand-in cannot show that real TeX prints them in that format. The
container image carries real `pdflatex` and `bibtex`. After a run, `latex.ok`
and `page_validation.body_pages` in `validation_report.json` show whether the
real build ran and how many body pages it counted.

The ICLR template needs `times`, `hyperref`, `url`, `eso-pic`, `fancyhdr` and
`natbib`; the writer usually adds `graphicx`, `booktabs` and `amsmath`. On
Debian the packages are `texlive-latex-base`, `texlive-latex-recommended`,
`texlive-latex-extra`, `texlive-fonts-recommended` and `texlive-science`, plus
`poppler-utils` for the vendored `validate_rendered_si.py` script.

### The claims check on the original's paper

`claims::check_paper` was run, read-only, on the paper folder of the original's
circle packing run. The command:

```
CLAIMS_CHECK_PAPER_DIR=<paper folder> cargo +1.97.0 test -p polaris_scientist \
  --test evidence by_hand -- --ignored --nocapture
```

All 20 claims of that paper resolve. Four of them cite 30 reference ids
(`ref.openalex.W…`) that only the paper's `references_provenance.json` lists, so
the check reads that file as well as `paper_data.json`. The original's last
audit record has verdict `fail` with three findings.

The check looks at every number the manuscript prints. On the original's paper
it finds 143 printed numbers, of which 62 are backed (a claim states them and a
cited source holds them, with the same sign) and 81 are not: 59 are stated by a
claim whose sources hold no such value (ratios, spreads and the problem size
`26`, which the original's `paper_data.json` does not list as a source), 17 are
stated by no claim and 5 by a claim about other text. The backed numbers include
the best score, the control mean and the baseline mean, each equal to its source
after rounding to the printed digits.

On the paper of a circle packing campaign of this system, kept in
`tests/fixtures/circpack_run/paper`, it finds 436 printed numbers, 31 backed and 405
not: the claims left after the repairs state only the scores, while the
abstract, the introduction, the formulas, the tables and the appendix print
counts and settings that no claim states.

In a run, the same rule is a validation error in the paper check, grouped by
sentence or table row, and goes to the repair step. A number that no recorded
value gives (a ratio the writer computed) is backed only by a recorded
derivation, because a number nobody can check is what the check is for.

## Differences from the original

Each item gives the reason.

### How roles work

1. **Roles work in copies.** Every role call gets its own folder with a
   read-only copy of the paper folder at `inputs/paper/`, the venue template
   at `inputs/venue/` (writers only) and an empty `outputs/`. The phase copies
   the acceptable files from `outputs/` into the paper folder: regular files
   only, no links, within fixed size and count limits, and never a file the
   phase's own systems write. The original's agents worked in the paper
   folder. Reason: what a role can change is small and checkable.
2. **The auditor and the reviewer cannot change the paper.** They have no edit
   grant and read the copy. The auditor may run `ls`, `cat`, `rg`, `diff`
   and `python` to compare the manuscript with `paper_data.json`, as the
   original's auditor ran commands to check numbers; the reviewer has no
   commands. The phase hashes the paper inputs before the call and after
   it; a change fails the step with the names of the files, whatever a
   command did. The original trusted these roles not to edit.
3. **Resume replays the staging.** `paper.stage` removes the files it
   generated, resets the counters of the phase and rebuilds the staged
   evidence. Finished model calls are restored from the step journal, so the
   same decisions are made in the same order. Files that are not generated
   (for example `ai_review.json`) are kept.

### Checks

4. **A system decides, not an agent.** The number check, the citation and
   provenance checks, the figure checks, the parse of the audit and the
   decision after each round are code. The original leaves these to an agent.
5. **The number check is new.** Every number the manuscript prints must be
   backed. The check reads `paper.tex` and every file it imports with `\input`
   or `\include` (tables, macro files), the body, the captions, the title and
   the cells of tables. A printed number is backed when (a) a claim in
   `claims_grounding.json` states it with the same sign and digits, in a
   sentence that shares content words with the sentence or table cell where the
   number is printed (a table cell needs no sentence of its own: a claim about
   the table that cites the source holding its values covers it), and (b) a
   source the claim cites holds it, as its `value` or as one entry of its
   `values`, after rounding to the printed digits, or a derivation the claim
   records recomputes to it. A derivation is an arithmetic `expression` over
   named `inputs`, each a recorded source (`id` or `id#path`); the check
   evaluates it, and refuses one whose result does not change when the inputs
   change (a bare `999`, or `a * 0 + 999`). Values of neighbouring sources and constants of the task count
   only when the claim cites them (`task.constants.*` are sources). Signs are
   compared; a number followed by `%` also matches the value times one hundred.
   Not checked: section, figure, table and equation numbers, years, citation
   numbers, TeX lengths and layout arguments, numbers glued to letters,
   exponents and subscripts of formulas, the integers 0, 1 and 2 inside math,
   and integers under a column header `i`, `j`, `idx`, `index`, `id` or `#`.
   A command that prints another file without `\input` or `\include`
   (`\subfile`, `\lstinputlisting`, `\verbatiminput`, `\csvautotabular` and a
   few more) is an error, because its numbers would go unread. A number
   with no backing is a hard error of the paper check, grouped by sentence or
   table row with the file and line, and goes to the repair step; the full list
   is `unbacked_numbers` in `validation_report.json`. A repair cannot hide a
   number by deleting it from a claim, because the check reads the text. The
   check uses only files the original also writes (`paper.tex`,
   `claims_grounding.json` with `claim` and `source_ids`, `paper_data.json`
   with `grounding_sources[*].value`); `derivations` and `values` are optional
   and add ways to back a number. The report (`claims_check.json`) has a
   `mechanical_check` that this run computed and a `saved_model_audit` that is
   only copied from `paper_evidence_audit.json`; the check does not run the
   audit. The `passed` verdict of the `mechanical_check` needs every claim in
   `claims_grounding.json` to resolve (it names a source or citation key, and
   each one it names exists) as well as every number to be backed;
   `unresolved_claims` lists each claim that does not, with its id, its text and
   the ids or keys that are missing. The check shows that each number is a
   recorded value or recorded arithmetic. It does not show that the sentence
   says something true about it; the evidence audit judges meaning.
6. **One repair counter.** A missing manuscript, a validation error, an audit
   finding and a planned review each draw on `paper.repair_round`, capped by
   `paper.max_repair_rounds`. The query plan, the strategy, the ranking, the
   gap analysis and the audit retries have counters of their own. The same
   validation failure twice with no change ends the phase `Incomplete`.
7. **An audit that cannot be read does not gate.** After two retries that tell
   the auditor what was wrong, the audit is recorded as `audit_degraded` in
   `paper_evidence_audit.json`, `paper_manifest.json` and `claims_check.json`.
   It is never called passed. A real finding does gate. A call that fails for
   an operational reason ends the phase `Incomplete`.
8. **Figure layout problems are warnings.** The original repairs figure
   quality with another agent round; that round is not built. Here a missing
   or invalid PNG or PDF, a bad figure type, an empty or unknown source id and
   a manifest that disagrees with the TeX are errors. Panel counts, category
   budgets, label and font sizes, and small or raster-only images are listed
   as warnings and never block. A manifest's figure type must be a supported
   one and equal the strategy's (after reading `flow_diagram` as `diagram` and
   `horizontal_bar` as `bar`). No manifest could satisfy a strategy whose type
   is free text outside the supported list, so the strategy check rejects such
   a figure spec and names the allowed types; the strategist corrects its own
   plan before any paper is written.
9. **No concept figure.** No image model is called. The figure manifest of a
   paper without an overview diagram says
   `"concept_figure": {"status": "none", "reason": …}`, and the check accepts
   it. The vendored manifest schema has no concept-figure generator.

### Retrieval and TeX

10. **Two reference services.** Semantic Scholar, then OpenAlex. The original's
    third source, the `scholarly` package, is not built: it scrapes a web
    page, which breaks without notice and is not covered by a stand-in.
11. **Provenance without time stamps.** `references_provenance.json` holds no
    `generated_at` or `retrieved_at`, so the same answers give the same files.
    It adds `attempts`: one record for each service asked in each round
    (`plan` or `postdraft`), with what happened. Keys and `ref.s2.…` /
    `ref.openalex.…` ids are stable.
12. **The TeX build is a system.** It runs `pdflatex`, `bibtex`, `pdflatex`,
    `pdflatex` in the paper folder with a scrubbed environment, its own process
    group, no shell escape and one deadline (`paper.latex.timeout_seconds`,
    default 300). A timeout stops the whole process group. Body pages come
    from probe lines the build adds to the log, not from the PDF. Only
    `paper.latex.command` and `paper.latex.timeout_seconds` are settings; the
    bibliography program is `bibtex` from `PATH`, because the configuration
    defaults do not list a key for it.
13. **Source ids are this system's.** `paper_data.json` is built from the run
    folder. Its ids are `result.*` (including `result.top_program_NN` and
    `result.improvement_NNN`), `ablation.<id>.score`,
    `ablation.control.<id>.score`, `posthoc.<id>.summary` and
    `posthoc.<id>.data.<file name without extension>`. The original names its
    ablation sources from study and variant names. The claims check looks ids
    up in each paper's own `paper_data.json`, so it reads both.

    The file also carries the original's `presentation_contract`,
    `evidence_sources`, `improvement_trace`, `top_programs` and `all_programs`,
    filled from the attempt history, and keeps the fields of this system
    (`task`, `totals`, `best_program`, `baseline`, `ablations`,
    `posthoc_analyses`, `final_evaluation`, `inputs`). Differences: it holds no
    time stamp and no absolute path; the rows of `all_programs` have no
    `program_dir` or `evaluation_evidence`; and `task.constants` holds the
    numbers the evaluator reports as public for the baseline run (the number of
    circles); each is also a source `task.constants.<name>`. Staging adds the
    numeric constants of the staged best program (`program.constants.<NAME>`)
    and the numbers of each post-hoc data file (`values` of
    `posthoc.<id>.data.<stem>`) so that a paper can cite them.
    The `baseline` block is written when the official scorer accepted the
    baseline run: a finite score in `task/baseline/metrics.json` and
    `correct: true` in `task/baseline/correct.json`. `baseline_provenance`
    records the score file, the verdict file and the scorer's own record
    (`evaluation_manifest.json`, with the evaluator hash). The packing is not
    judged again: the scorer allows circles of radius zero, which may share a
    centre. Without a finite score and a passing verdict the block and the
    `baseline.*` sources are left out and `limitations` says that no baseline
    comparison is made.

    For every variant and control of the ablation pipeline, staging also
    writes three small files under `dossier/ablations/variants/<id>/` and one
    record for each in `paper_data.json.ablation_records`:
    `variant_spec.json` (the specification), `source.diff` (a unified diff of
    the source of the last evaluated program against the source of the
    control's program, three lines of context, cut at 64 KiB with a marker)
    and `verification.json` (the verifier's `rubric_status.json` of that
    program). The last evaluated program is the highest `program_N` with a
    `results` folder. The reference is the control's `program_0`, the winner
    the ablations start from, for every variant including the seed baseline;
    the seed baseline's own `program_0` is the seed, not the winner, so it is
    never the reference. When the run has no control, a variant with a
    later program is compared with its own `program_0`. The control has no
    diff and no verification record.
    A variant whose source equals the control's gets a `source.diff` that says
    so, and a variant without a verification record (an unedited program, or
    a record that is not valid JSON with `overall_pass`) is listed with
    `found: false` and the reason. Only `.py` files and the entrypoint named
    in `program_spec.toml` are compared; folders such as caches and virtual
    environments are skipped, nothing is followed through a link, and an id
    that is not a plain folder name gets no files. The original stages a
    whole task bundle for each variant; the diff is the part a reader needs
    to check a description, at a size a role can read.

### Review

14. **The presentation review is optional.** `paper.review_rounds` is 0 by
    default. A round runs a read-only reviewer and, when it asks for changes,
    a repair that draws on the repair counter. A reply that breaks the
    contract after the retries is dropped and the phase goes on, as the
    original does.
15. **The outside reviewer and the revision are not built.** With
    `review.mode = "off"` the review phase writes `ai_review_status.json` with
    status `skipped` and the reason. With `adapter` it writes `deferred`; the
    review runs after the campaign on `paper.pdf`. When `paper/ai_review.json`
    exists, the revision step writes `revision_status.json` with status
    `not_built` and changes nothing. The review phase does not run after an
    incomplete paper.
16. **A smaller output limit per request.** The original's Claude Code 2.1.285
    sends `max_tokens` 128000 with adaptive thinking on every request. Here
    the writer, the repairer and the editor send 32000 and every other role
    sends the runner's 16384 (`runner.max_output_tokens`). Reason: the cost
    guard reserves a request's prompt and its whole output limit at the
    output price against the call's dollar ceiling, and a 128000-token limit
    would use up most of the smaller ceilings before any prompt is counted.
    A role sets its limit with `max_output_tokens` in its role file or
    `roles.<key>.max_output_tokens`. The runner registers the model provider
    with a ceiling of 128000, because a request can lower the provider's
    ceiling but never raise it; each call then sends its own limit, the
    role's or `runner.max_output_tokens`. With the provider at the runner's
    16384, a role's larger limit reached only the cost guard and the
    request was still cut at 16384. The value 128000 comes from the CLI's
    model table and a request sent to a stand-in endpoint; whether the
    service applies its own override was not checked.
    The larger limit has a cost: the guard stops a call when its spend plus
    the worst case of the next request would pass the ceiling, and the worst
    case now includes 32000 output tokens. A writer can therefore stop before
    it has written `paper.tex`. The draft step then finds no manuscript, so
    the gap analysis, the post-draft plan and the enrichment (all guarded on
    a drafted manuscript) are skipped, and the first repair round writes the
    manuscript from the template. This happened in one paper-phase run; in
    two others the writer wrote the manuscript before its ceiling. This is
    one sample each, so the stop is reported and the ceilings are left as
    they are.
17. **`create_file` does not replace a file.** The tool refuses a path that
    exists, and the original's step list tells the writer to copy
    `paper.tex` from the template first. A writer that copied it and then
    sent the whole manuscript to `create_file` paid for the manuscript
    twice. The writer, repairer and editor prompts therefore say that
    `create_file` only makes new paths, that a copied file is changed with
    `edit_file`, that `paper.tex` is copied only to be edited, and that a
    whole replacement is made under a new name and copied over with `cp`.
    In a writer-only run with these prompts the manuscript was written in one
    successful `create_file` call.
18. **The audit checks each description of a variant.** The original's audit
    prompt asks for inspectable source evidence for implementation-specific
    claims and says that unsupported details are removed or qualified; it has
    no wording about the baseline, ablation and control descriptions in
    particular, and its dossier holds no diff. Here the audit prompt adds one
    paragraph and one line in its failing examples: every description of a
    baseline, an ablation variant or the control is checked against the
    `source.diff` and `verification.json` of that variant, a description that
    only the spec supports is removed or qualified, and a mismatch is a
    finding of the usual form. A finding goes through the same repair counter
    as any other audit finding (item 6); the repair feedback carries its
    issue, evidence and fix. The writer, enricher, editor and repairer
    prompts name the three files and say that a description must agree with
    the diff. The check is the auditor's judgement, not a system check: no
    code compares prose with a diff.

    One audit call was run on a saved accepted paper, with the variant files
    staged from its saved run. The paper described its single-start baseline
    as having no LP polish; the variant's diff keeps the `_polish(cen)` call.
    The auditor returned a FAIL whose first finding named that mismatch and
    quoted the diff lines and the spec sentence "with the LP polish". It
    also found that the no-LP variant replaces the exact LP by the seed's
    proportional-scaling radii, not by the raw solver radii. This is one
    sample of a model's judgement; it shows the files are enough to find a
    mismatch, not that every mismatch is found.

## Prompt edits

The prompts, the skills and their scripts come from AI-Scientist-v3 under the
Apache License 2.0. `assets/prompts/paper/NOTICE` lists every change, one line
per file, and each Markdown, Python and shell file carries a two-line header
with its original path and its changes. A test checks the headers.

In short:

- The memory protocol, the global memory input, the rubrics section, the
  contributions scaffold, the paper edit context and every use of an image
  model or an API key were removed.
- Paths use the placeholders of this phase (`inputs/`, `outputs/`, `skills/`).
- The final-evaluation paragraph describes this system's `paper_data.json`.
- The advice to install tools became "use only what is installed", because the
  agent has no network.
- The review and the audit are replies in the final message, not files.
- Not copied: the concept figure skill (needs an image model), the memory
  skill, the reference retrieval skill (the host retrieves references), the
  `write-paper` skill (the standalone one is used), the other venue template,
  and `validate_figures.py` (imports modules of the original repository).

A test checks that no vendored prompt names a feature that is not built.

## Not built or not verified

- The revision after an outside review, the figure-quality repair and the
  concept figure (all report that they are not built or are absent).
- The `scholarly` fallback.
- A real `pdflatex` build inside the tests (they use a stand-in; see "TeX").
- A run against the real reference services. Only the two requests above were
  made, by hand.
- The role ceilings in `assets/roles/evidence.toml` are starting values. A
  ceiling that is too low shows as `cost_cap` stops in `cost_events.jsonl`.
