# Circle-packing campaign records

Download: [scientist-records.tar.gz](https://github.com/RadiantAILabs/auditable-scientist/releases/download/v0.1.0/scientist-records.tar.gz). The bundle contains three
campaigns: `polaris-reported`, `polaris-first-start`, and `original`. It also
contains their outside evaluations and paper reviews, saved comparison outputs,
and the article and figure inputs checked by [TRACE.md](TRACE.md).

The original campaign resumed once. The first Polaris start ended with an
incomplete paper after a provider failure during an evidence audit; the reported
start completed. These are individual samples, not a controlled speed comparison.

The Polaris campaigns were run with a build made before this release; its
`run.json` reports version `0.2.0`. The released code (version 0.1.0) has the
same graph, checks, limits and record formats. It differs in four ways that came
with the move to the released `polaris_code`: the `total` of a `phase_start`
event counts the review phase, variant and analysis ids that are all digits or
`_abandoned` are refused, an expected output larger than 64 MiB is refused, and
a reply that reports only one token count is charged at the call's estimate.

Archive size: **24,827,724 bytes (23.68 MiB)**. The exact SHA-256 checksum is in
[SHA256SUMS](SHA256SUMS).

## Download and verify

```sh
curl -L 'https://github.com/RadiantAILabs/auditable-scientist/releases/download/v0.1.0/scientist-records.tar.gz' -o scientist-records.tar.gz
sha256sum -c records/SHA256SUMS
tar -xzf scientist-records.tar.gz
python3 -I records/reproduce.py scientist-records --out measurements.json
python3 -I records/fit_prompts.py scientist-records
python3 -I records/check_article.py scientist-records measurements.json
python3 -I records/scrub.py scientist-records
uv run --script records/scan_pdf.py scientist-records
```

Counting, fitting and byte-scanning use the Python 3.11 standard library. The
additional PDF text/metadata scan uses a `uv` script pinned to `pypdf==6.1.1`.
The scripts read
saved data; they do not run candidate programs, evaluators, model services, or
commands found in conversations. The reproduction command verifies every file
listed in `manifest.json` before computing results. Archive file hashes refer to
the publication copies. Hashes embedded in historical audits and comparison
outputs refer to the files as originally saved, before the publication scrub;
they are evidence about those runs, not replacement archive checksums.

## Contents

- `campaigns/`: event timelines, cost ledgers, resolved campaign and role settings,
  scientific-agent conversations, program sources, evaluation records and arrays,
  ablations, analyses, final paper source and PDF, validation and evidence audits.
- `checks/`: saved version 4 mechanical claim-check reports and run summaries.
- `reviews/`: saved individual reviews, chair decisions, summaries and call meters.
  The original paper's review was reused from the earlier comparison of that same
  PDF. Each reviewed PDF hash is checked by `reproduce.py`.
- `rescores/`: three outside executions of each winner, including correctness,
  metrics and saved packing arrays. These are separate from campaign costs.
- `host-logs/`: the two Polaris campaigns' saved host logs, including provider
  retries and their accounting messages.
- `comparison/`: saved token, prompt and action observations used by the article.
- `article/`: the post as published and its figure source data, preserved as provenance.
- `implementation/`: supporting source for recorded prices and policy checks.
- `manifest.json`: relative names, byte lengths and SHA-256 hashes of bundle files.

A small earlier-ablation source excerpt supports the historical anecdote; its
byte comparison is separate from all three campaign totals.

Scientific-agent prompts and conversations are records of the experiment and
are included. Instructions and conversations used to develop the software are
excluded. No missing original conversation is reconstructed from a later run.
Saved session-history files are retained where available as additional evidence;
coverage counts require a conversation whose usage matches the billed call.

## Exclusions

Python environments, package caches, wheels, bytecode, version-control metadata,
lock files, runtime pipes and runtime markers are rebuildable machinery. Repeated frozen `inputs/`
trees and state-store snapshots duplicate the candidate and evidence records kept
elsewhere. LaTeX temporary files and rebuildable intermediate images are omitted;
the final paper PDF and its source figure assets are retained. Symlinks are not
followed or archived. Account/session support files are omitted; scientific
conversation JSONL files are retained. Unrelated campaign directories, smoke tests, replay
experiments, software-development prompts, planning documents, decisions, notes,
coordination files and development-agent transcripts are outside this bundle.

## Publication scrub

[scrub.py](scrub.py) contains the readable rules. Text files remain as saved except
for deterministic credential redaction and identity substitution. JSON and logs
are scanned as bytes, including nested escaped text. Binary records are checked
before inclusion; any binary needing a substitution stops the build. ZIP members (including packing arrays) and PDF Flate
streams are also decompressed for scanning. `scan_pdf.py` additionally decodes
every PDF page and its document metadata for the same scan rules. Character-map hex glyph encodings are
excluded from account-ID matching because they are PDF font syntax.

Credential rules search AWS access-key IDs, secret/session-token assignments,
API/auth token and password assignments, bearer credentials, signed-URL credential
parameters, private-key markers, JWTs, and common provider/GitHub/Slack key forms.
Matched values become `<REDACTED>`; reports contain file names and rule names only.

Identity rules replace ARNs with `<ARN>`, standalone 12-digit account identifiers
with `<ACCOUNT_ID>`, private/loopback IPv4 addresses with `<PRIVATE_IP>`, cloud host
names with `<CLOUD_HOST>`, home-directory account prefixes with `<HOME>`, task-directory
prefixes with `<TASK_PATH>`, and local host aliases with `<LOCAL_HOST>`. Exact
campaign-root paths become archive-relative `campaigns/<campaign>` paths first.
Historical campaign aliases become the three public campaign names. These rules
preserve relative candidate paths and evidence keys used by the counting scripts.

A clean scan is a documented result of these rules, not a proof that arbitrary
unrecognized secrets cannot exist. The finished archive is extracted and scanned
again; [SCAN.json](SCAN.json) records that result.

## Interpreting the measurements

Ledger dollars are saved estimates, not a cloud invoice. Token dollars apply the
recorded rates per million tokens: $2 fresh input, $2.50 cache writes, $0.20 cache
reads and $10 output. The difference between token dollars and ledger dollars
includes reservation accounting and rounding; records do not establish an actual
invoice for failed attempts. External reviews are excluded from campaign totals.

Polaris meter requests include retries; observed usage responses count successful
responses with recorded token usage. Original native turns use another definition.
Fixed-prompt sizes are regression estimates from saved observations. Their fit
assumptions and residual errors are printed by `fit_prompts.py`.

The mechanical claim checker counts printed numeric occurrences it recognizes. It
has exemptions and cannot certify that every scientific claim is true. A passing
saved model audit is another distinct observation; it can become stale after an
edit. See the per-claim qualifications and discrepancies in [TRACE.md](TRACE.md).
