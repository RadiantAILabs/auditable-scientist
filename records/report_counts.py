#!/usr/bin/env python3
"""Report saved campaign measurements with provenance, without executing run code.

Uses only the standard library; invoke with python3 -I. Input directories are
read-only. Missing counters are reported as unknown rather than inferred from
sessions, token counts, a saved model audit, or a ledger annotation.
"""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
from datetime import datetime
from decimal import Decimal
import csv
import hashlib
import io
import json
from pathlib import Path
import re


DEFINITIONS = {
    "sessions": "One distinct top-level metrics/cost_events.jsonl event_id is one recorded role invocation, including zero-cost interrupted invocations. Nested ledgers are never added. Phase attribution uses the latest phase_start at or before the cost timestamp; ledger phase labels can name sub-stages instead. Role counts strip only a trailing _round_N from native stage names; other names remain unchanged.",
    "retained": "Accounting reconstruction, not a clean-run measurement or certification of scientific validity: keep calls in the last positive-duration closed occurrence of each phase, including failed final paper work. Zero-duration replay/skip records do not replace real work. If a phase has no closed occurrence, keep its latest started occurrence and mark it incomplete. Earlier occurrences are discarded. A later unclosed occurrence supersedes an earlier closed one. All paid work remains in total dollars.",
    "requests": "Polaris agent_end.fields.turns is the session meter Usage.requests counter. Count it once per ended session. A session is not an inference request. Reference logs do not expose an equivalent trusted request counter; unknown is not zero.",
    "time": "Total event span is first to last pipeline timestamp; total observed span also includes top-level cost timestamps. Neither establishes an unrecorded process stop. Closed-phase time sums recorded elapsed_seconds, falling back to elapsed strings (rounded). Retained phase time excludes final_evaluation to preserve the report's task/investigation/ablations/posthoc/paper convention; the inclusive total is also given. Agent seconds sum paired session intervals and can overlap.",
    "idle": "Event silences exceed the configured threshold between consecutive pipeline timestamps. A silence before pipeline_start establishes a measured between-segment gap only when the preceding segment has pipeline_end (allowing trailing bookkeeping events). Silences during agents/phases or with a missing pipeline_end are not proven idle. Heartbeats and event granularity affect these observations.",
    "money": "Decimal sums of run-native cost estimates and separate ledger settlements; neither is an invoice. Campaign ledger IDs are explicit inputs, never selected by an ambiguous run name. Ledger notes are annotations, not phase/round evidence.",
    "paper": "Saved model audit status is reported, not re-executed. Validation snapshots and session/log statuses are distinct. A requested repair round is not a completed repair. PDF pages come from validation or the saved LaTeX log, never an estimated byte-to-page conversion.",
}


def decimal_sum(values):
    return sum((Decimal(str(x)) for x in values), Decimal(0))


def instant(value):
    result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if result.tzinfo is None:
        raise ValueError(f"timestamp lacks timezone: {value}")
    return result


def seconds(a, b):
    return Decimal(str((instant(b) - instant(a)).total_seconds()))


def duration(fields):
    if "elapsed_seconds" in fields:
        return Decimal(str(fields["elapsed_seconds"]))
    text = fields.get("elapsed", "")
    parts = re.findall(r"(\d+(?:\.\d+)?)\s*(h|m|s)", text)
    return decimal_sum(Decimal(n) * {"h": 3600, "m": 60, "s": 1}[u] for n, u in parts) if parts else None


class Report:
    def __init__(self):
        self.facts = []
        self.inputs = {}
        self.contents = {}
        self.warnings = []

    def source(self, path, key="$", line=None):
        path = Path(path).resolve()
        if path.is_file() and str(path) not in self.inputs:
            self.contents[str(path)] = path.read_bytes()
            self.inputs[str(path)] = hashlib.sha256(self.contents[str(path)]).hexdigest()
        result = {"file": str(path), "key": key}
        if line is not None:
            result["line"] = line
        if not path.is_file():
            result["missing"] = True
        return result

    def read(self, path):
        self.source(path)
        return json.loads(self.contents[str(Path(path).resolve())])

    def lines(self, path):
        self.source(path)
        return list(enumerate(self.contents[str(Path(path).resolve())].decode().splitlines(), 1))

    def rows(self, path):
        return [(n, json.loads(line)) for n, line in self.lines(path) if line.strip()]

    def fact(self, metric, value, sources, note=""):
        self.facts.append({"metric": metric, "value": value, "sources": sources, "note": note})
        return value

    def field(self, prefix, path, key, default=None):
        data = self.read(path) if Path(path).is_file() else {}
        value = data
        for part in key.split("."):
            value = value.get(part, default) if isinstance(value, dict) else default
        return self.fact(prefix, value, [self.source(path, key)])


def summarize_run(report, name, root, threshold, claims=None, rescores=(), paper_log=None, guard_log=None):
    root = root.resolve()
    ep, cp = root / "pipeline_events.jsonl", root / "metrics/cost_events.jsonl"
    events, cost_rows = report.rows(ep), report.rows(cp)
    if not events:
        raise ValueError(f"empty event timeline: {ep}")
    if any(instant(a[1]["timestamp"]) > instant(b[1]["timestamp"]) for a, b in zip(events, events[1:])):
        raise ValueError(f"non-monotonic event timeline: {ep}")
    # Repeated IDs sometimes come from append/recovery. Count identical events once,
    # reject conflicting payloads, and preserve every original line as evidence.
    unique, cost_sources = {}, defaultdict(list)
    for n, row in cost_rows:
        eid = row["event_id"]
        if eid in unique and unique[eid] != row:
            raise ValueError(f"conflicting cost event_id {eid}: {cp}:{n}")
        unique[eid] = row
        cost_sources[eid].append(report.source(cp, "$", n))
    costs = list(unique.values())
    report.fact(f"{name}.duplicate_cost_rows", len(cost_rows) - len(costs), [report.source(cp)])

    starts = [(n, e) for n, e in events if e["event"] == "pipeline_start"]
    if not starts:
        raise ValueError(f"no pipeline_start: {ep}")
    occurrences, active, segment = [], {}, 0
    for n, e in events:
        if e["event"] == "pipeline_start":
            segment += 1
            active = {}
        phase = e.get("fields", {}).get("phase_name", e.get("phase"))
        if e["event"] == "phase_start":
            occurrence = {"phase": phase, "segment": segment, "start": e["timestamp"], "start_line": n,
                          "end": None, "elapsed": None, "sources": [report.source(ep, "fields", n)]}
            occurrences.append(occurrence)
            active[phase] = occurrence
        if e["event"] == "phase_end":
            elapsed = duration(e["fields"])
            occurrence = active.pop(phase, None)
            if occurrence is None:
                # A replay or a skipped phase has no phase_start and no new work.
                occurrence = {"phase": phase, "segment": segment, "start": None, "start_line": None,
                              "sources": []}
                occurrences.append(occurrence)
            occurrence.update(end=e["timestamp"], elapsed=elapsed, status=e["fields"].get("status"))
            occurrence["sources"].append(report.source(ep, "fields", n))
    keep = {}
    for occurrence in occurrences:
        if occurrence["start"] is not None and (occurrence["elapsed"] is None or occurrence["elapsed"] > 0):
            keep[occurrence["phase"]] = occurrence
    for phase, o in keep.items():
        report.fact(f"{name}.retained_occurrence.{phase}", {k: o.get(k) for k in ("segment", "start", "end", "elapsed", "status")}, o["sources"])

    phase_starts = [(n, e) for n, e in events if e["event"] == "phase_start"]
    attributed = []
    for c in costs:
        before = [(n, e) for n, e in phase_starts if instant(e["timestamp"]) <= instant(c["timestamp"])]
        if not before:
            raise ValueError(f"cost event precedes every phase: {c['event_id']}")
        n, start = before[-1]
        phase = start["fields"]["phase_name"]
        retained = phase in keep and keep[phase]["start_line"] == n
        seg = sum(instant(e["timestamp"]) <= instant(c["timestamp"]) for _, e in starts)
        attributed.append({"cost": c, "phase": phase, "role": re.sub(r'_round_\d+$', '', c.get("stage", "unknown")), "retained": retained,
                           "segment": seg, "sources": cost_sources[c["event_id"]] + [report.source(ep, "fields.phase_name", n)]})

    def accounting(prefix, calls):
        refs = [s for c in calls for s in c["sources"]] or [report.source(cp, "event_id"), report.source(ep, "event/fields.phase_name")]
        report.fact(prefix + ".sessions", len(calls), refs)
        report.fact(prefix + ".usd", decimal_sum(c["cost"]["total_cost_usd"] for c in calls), refs)

    for scope in ("all", "retained", "discarded"):
        calls = [c for c in attributed if scope == "all" or c["retained"] == (scope == "retained")]
        accounting(f"{name}.{scope}", calls)
        report.fact(f"{name}.{scope}.sessions_by_role", dict(sorted(Counter(c['role'] for c in calls).items())),
                    [s for c in calls for s in c['sources']] or [report.source(cp)])
        for phase in sorted({o['phase'] for o in occurrences} | {c['phase'] for c in calls}):
            subset = [c for c in calls if c["phase"] == phase]
            accounting(f"{name}.{scope}.{phase}", subset)
            roles = Counter(c["role"] for c in subset)
            report.fact(f"{name}.{scope}.{phase}.sessions_by_role", dict(sorted(roles.items())),
                        [s for c in subset for s in c["sources"]] or [report.source(cp), report.source(ep)])
    report.fact(f"{name}.models", dict(Counter(c.get("model", "unknown") for c in costs)), [report.source(cp, "model")])
    report.fact(f"{name}.cost_basis", dict(Counter(c.get("cost_basis", "unknown") for c in costs)), [report.source(cp, "cost_basis")])
    report.fact(f"{name}.tokens", dict(sorted({k: sum(c.get('usage', {}).get(k, 0) for c in costs)
                                             for k in {k for c in costs for k in c.get('usage', {})}}.items())), [report.source(cp, "usage")])
    summary = root / "metrics/cost_summary.json"
    if summary.is_file():
        native = report.field(f"{name}.cost_summary_usd", summary, "total_cost_usd")
        if native is not None and Decimal(str(native)) != decimal_sum(c['total_cost_usd'] for c in costs):
            report.warnings.append(f"{name}: cost summary differs from distinct cost events")

    agent_ends = [(n, e) for n, e in events if e['event'] == 'agent_end']
    # Presence of any ended session does not make absent counters zero.
    complete = (bool(agent_ends) and all('turns' in e['fields'] for _, e in agent_ends)
                and len(agent_ends) == len(costs)
                and len(agent_ends) == sum(e['event'] == 'agent_start' for _, e in events))
    for phase in [None] + sorted({c['phase'] for c in attributed}):
        selected = [(n, e) for n, e in agent_ends if phase is None or e.get('phase') == phase]
        report.fact(f"{name}.requests" + (f".{phase}" if phase else ""),
                    sum(e['fields']['turns'] for _, e in selected) if complete else None,
                    [report.source(ep, "fields.turns", n) for n, _ in selected] or [report.source(ep, "agent_end.fields.turns")],
                    "Meter request count; not session count." if complete else "Equivalent request counter unavailable.")
    opened, paired = {}, []
    for n, e in events:
        if e['event'] == 'agent_start':
            opened[(e['fields']['step'], e['fields'].get('workspace'))] = (n, e)
        if e['event'] == 'agent_end':
            start = opened.pop((e['fields']['step'], e['fields'].get('workspace')), None)
            if start:
                paired.append((seconds(start[1]['timestamp'], e['timestamp']), start[0], n))
    report.fact(f"{name}.agent_seconds", decimal_sum(p[0] for p in paired) if paired else None,
                [report.source(ep, 'timestamp', n) for p in paired for n in p[1:]] or [report.source(ep, "agent_start/agent_end")])
    report.fact(f"{name}.paired_sessions", len(paired), [report.source(ep, "agent_start/agent_end")])
    report.fact(f"{name}.unpaired_agent_starts", len(opened), [report.source(ep, "agent_start/agent_end")])
    report.fact(f"{name}.event_span_seconds", seconds(events[0][1]['timestamp'], events[-1][1]['timestamp']),
                [report.source(ep, 'timestamp', events[0][0]), report.source(ep, 'timestamp', events[-1][0])])
    final_end = next(((n, e) for n, e in reversed(events) if e['event'] == 'pipeline_end'), None)
    final_start = starts[-1]
    report.fact(f"{name}.total_elapsed_seconds",
                seconds(starts[0][1]['timestamp'], final_end[1]['timestamp']) if final_end and final_end[0] > final_start[0] else None,
                [report.source(ep, 'timestamp', starts[0][0]), report.source(ep, 'pipeline_end.timestamp', final_end[0] if final_end else None)],
                'Across all resumes; unknown if the final launch lacks pipeline_end.')
    timestamps = [(e['timestamp'], report.source(ep, 'timestamp', n)) for n, e in events]
    timestamps += [(c['timestamp'], cost_sources[c['event_id']][0]) for c in costs]
    first, last = min(timestamps, key=lambda x: instant(x[0])), max(timestamps, key=lambda x: instant(x[0]))
    report.fact(f"{name}.observed_span_seconds", seconds(first[0], last[0]), [first[1], last[1]], "Includes cost events; unrecorded shutdown time remains unknown.")
    closed = [o for o in occurrences if o.get('elapsed') is not None]
    report.fact(f"{name}.closed_phase_seconds", decimal_sum(o['elapsed'] for o in closed), [s for o in closed for s in o['sources']])
    for phase in sorted({o['phase'] for o in occurrences}):
        subset = [o for o in closed if o['phase'] == phase]
        report.fact(f"{name}.closed_phase_seconds.{phase}", decimal_sum(o['elapsed'] for o in subset), [s for o in subset for s in o['sources']] or [report.source(ep)])
    kept = [o for phase, o in keep.items() if phase != 'final_evaluation']
    for label, subset in [('retained_phase_seconds', kept), ('retained_phase_seconds_including_final', list(keep.values()))]:
        report.fact(f"{name}.{label}", decimal_sum(o['elapsed'] for o in subset) if all(o['elapsed'] is not None for o in subset) else None,
                    [s for o in subset for s in o['sources']], 'Incomplete phase duration stays unknown.')
    silences, measured_idle = [], []
    segment_closed = False
    closed_line = None
    for (n, a), (m, b) in zip(events, events[1:]):
        if a['event'] == 'pipeline_start':
            segment_closed = False
            closed_line = None
        elif a['event'] == 'pipeline_end':
            segment_closed = True
            closed_line = n
        gap = seconds(a['timestamp'], b['timestamp'])
        if gap > threshold:
            idle = segment_closed and b['event'] == 'pipeline_start'
            refs = [report.source(ep, 'timestamp', n), report.source(ep, 'timestamp', m)]
            if idle and closed_line != n:
                refs.append(report.source(ep, 'event', closed_line))
            silences.append({'seconds': gap, 'from': a['timestamp'], 'to': b['timestamp'], 'proven_idle': idle, 'sources': refs})
            if idle:
                measured_idle.append(gap)
    report.fact(f"{name}.event_silence_threshold_seconds", threshold, [report.source(__file__, 'CLI --idle-threshold-seconds')], 'Effective argument (default 120); silence must be strictly greater than threshold.')
    report.fact(f"{name}.event_silences", silences, [s for gap in silences for s in gap['sources']] or [report.source(ep, 'timestamp')])
    report.fact(f"{name}.measured_idle_gap_seconds", decimal_sum(measured_idle), [s for gap in silences if gap['proven_idle'] for s in gap['sources']] or [report.source(ep, 'pipeline_end/pipeline_start')], 'Sum of proven between-segment gaps over threshold; other idle time is unknown.')

    for i, (n, start) in enumerate(starts, 1):
        next_n = starts[i][0] if i < len(starts) else float('inf')
        window = [(m, e) for m, e in events if n <= m < next_n]
        ends = [(m, e) for m, e in window if e['event'] == 'pipeline_end']
        stop = ends[-1] if ends else None
        report.fact(f"{name}.segment.{i}", {
            'start': start['timestamp'], 'stop': stop[1]['timestamp'] if stop else None,
            'stop_reason': stop[1]['fields'].get('status') if stop else 'not recorded',
            'last_pipeline_observation': window[-1][1]['timestamp'],
            'seconds': seconds(start['timestamp'], stop[1]['timestamp']) if stop else None,
        }, [report.source(ep, '$', n), report.source(ep, '$', stop[0] if stop else window[-1][0])], 'No pipeline_end means unknown stop, not the next launch time.')
        accounting(f"{name}.segment.{i}", [c for c in attributed if c['segment'] == i])
        terminal = [(m, e) for m, e in window if e['event'] == 'paper_gate' and e['fields'].get('gate') == 'finalization' and e['fields'].get('state') != 'started']
        if terminal:
            m, e = terminal[-1]
            report.fact(f"{name}.segment.{i}.paper_terminal_gate", {'timestamp': e['timestamp'], **e['fields']}, [report.source(ep, 'fields', m)], 'Finalization status, not process shutdown time.')
        report.fact(f"{name}.segment.{i}.configuration", start['fields'].get('execution', start['fields']), [report.source(ep, 'fields', n)])

    paper_counts(report, name, root, events, paper_log)
    attempts = root / 'investigation/attempts_history.jsonl'
    if attempts.is_file():
        rows = report.rows(attempts)
        report.fact(f"{name}.investigation.recorded_attempts", len(rows), [report.source(attempts)], 'History rows, not inferred successful ideas.')
    for rel, keys in [('ablations/pipeline_summary.json', ('variants', 'replications_requested', 'replications_observed')),
                      ('posthoc/latest_manifest.json', ('analyses', 'rejected', 'selected_analysis_id'))]:
        path = root / rel
        if path.is_file():
            data = report.read(path)
            for key in keys:
                if key in data:
                    report.fact(f'{name}.{rel}.{key}', data[key], [report.source(path, key)])
            if isinstance(data.get('variants'), list):
                report.fact(f'{name}.ablations.variant_count', len(data['variants']), [report.source(path, 'variants')])
    csv_path = root / 'ablations/aggregated/run_summary.csv'
    if csv_path.is_file():
        report.source(csv_path)
        handle = io.StringIO(report.contents[str(csv_path.resolve())].decode(), newline='')
        reader = csv.DictReader(handle)
        header_lines = reader.line_num
        # DictReader reads its header lazily. Force it before recording row starts.
        reader.fieldnames
        previous_line = reader.line_num or header_lines
        for row in reader:
            report.fact(f'{name}.ablations.csv.{previous_line + 1}', row,
                        [report.source(csv_path, '$', previous_line + 1)], 'CSV fields preserve their recorded strings.')
            previous_line = reader.line_num
    if guard_log:
        observations = []
        for n, line in report.lines(guard_log):
            if '[guard]' in line and ('recorded' in line or 'exited' in line):
                observations.append({'text': line, 'source': report.source(guard_log, '$', n)})
        report.fact(f"{name}.guard_observations", observations, [o['source'] for o in observations] or [report.source(guard_log)])
    for stem in ('investigation/best_program_manifest.json', 'task/baseline/metrics.json', 'task/baseline/correct.json'):
        path = root / stem
        for key in (('combined_score', 'correct') if 'manifest' in stem else ('correct',) if 'correct' in stem else ('combined_score',)):
            report.field(f"{name}.{stem}.{key}", path, key)
    for folder in sorted(set(Path(p).resolve() for p in rescores) | {p.parent for p in (root / 'rescore').rglob('metrics.json')}):
        for key, filename in [('combined_score', 'metrics.json'), ('correct', 'correct.json')]:
            report.field(f"{name}.rescore.{folder}.{key}", folder / filename, key)
    if claims is None:
        claims = next((p for p in (root / 'paper/claims_check.json', root / 'claims_check.json') if p.is_file()), None)
    if claims:
        for key in ('checker_version', 'claims_listed', 'claims_resolved', 'numbers_checked', 'numbers_matching', 'audit'):
            report.field(f"{name}.check_claims.{key}", claims, key)
        data = report.read(claims)
        report.fact(f"{name}.check_claims.mismatches", len(data['mismatches']) if 'mismatches' in data else None,
                    [report.source(claims, 'mismatches')], 'Counts saved checker output; no new model audit.')
        for key in sorted(set(data) - {'checker_version', 'claims_listed', 'claims_resolved', 'numbers_checked', 'numbers_matching', 'audit', 'mismatches'}):
            value = data[key]
            report.fact(f'{name}.check_claims.{key}', len(value) if isinstance(value, list) else value,
                        [report.source(claims, key)], 'Additional checker fields; lists are counted, objects retain their named counters.')
    else:
        report.fact(f"{name}.check_claims", None, [report.source(root / 'paper/claims_check.json')], 'Pass a saved checker result with --*-claims to count it.')


def paper_counts(report, name, root, events, paper_log):
    paper = root / 'paper'
    for path in sorted((paper / 'rounds').glob('*/validation_report.json')) + sorted((paper / 'paper_diagnostics/validation').glob('round_*/attempt_*/validation_report.json')):
        label = path.parent.name if path.parent.parent.name == 'rounds' else path.parent.parent.name + '/' + path.parent.name
        data = report.read(path)
        report.fact(f"{name}.paper.validation.{label}", {k: data.get(k) for k in ('ok', 'error', 'claim_count', 'numbers_checked', 'numeric_mismatches')}, [report.source(path)])
        audit = path.parent / 'paper_evidence_audit.json'
        if audit.is_file():
            data = report.read(audit)
            report.fact(f"{name}.paper.audit.{label}", {k: data.get(k) for k in ('state', 'verdict', 'finding_count')}, [report.source(audit)])
    for n, e in events:
        fields = e.get('fields', {})
        if e['event'] == 'paper_gate' and fields.get('gate') in ('evidence_audit', 'repair', 'finalization'):
            report.fact(f"{name}.paper.gate.{n}", {'timestamp': e['timestamp'], **fields}, [report.source(root / 'pipeline_events.jsonl', 'fields', n)])
        if e.get('phase') == 'paper' and e['event'] in ('agent_end', 'step_skipped') and re.search(r'audit|repair', fields.get('step', '')):
            report.fact(f"{name}.paper.session.{n}", {k: fields.get(k) for k in ('step', 'role', 'stop', 'reason')}, [report.source(root / 'pipeline_events.jsonl', 'fields', n)])
    if paper_log:
        for n, line in report.lines(paper_log):
            match = re.search(r'(Evidence Audit|Repair|Finalization) · (started|completed|failed).*?(?:round=(\d+)\))?', line)
            if match:
                round_match = re.search(r'round=(\d+)', line)
                report.fact(f"{name}.paper.log.{n}", {'stage': match[1], 'status': match[2], 'round': int(round_match[1]) if round_match else None}, [report.source(paper_log, '$', n)])
        tokens = [(n, line) for n, line in report.lines(paper_log) if 'Tokens ·' in line and 'Time ' in line]
        if tokens:
            n, line = tokens[-1]
            logged = report.fact(f"{name}.paper.logged_seconds", duration({'elapsed': line.split('Time ')[-1]}), [report.source(paper_log, '$', n)], 'Rounded log elapsed, separate from pipeline event duration.')
            # Only the original's separate paper launch uses this convention.
            # Polaris pipeline_end elapsed is cumulative over resumes, so summing
            # its values would double-count time.
            ends = [(m, e) for m, e in events if e['event'] == 'pipeline_end']
            starts = [(m, e) for m, e in events if e['event'] == 'pipeline_start']
            if len(starts) == 2 and len(ends) == 1 and ends[0][0] < starts[-1][0] and logged is not None:
                report.fact(f"{name}.reported_active_seconds", duration(ends[0][1]['fields']) + logged,
                            [report.source(root / 'pipeline_events.jsonl', 'fields.elapsed_seconds', ends[0][0]), report.source(paper_log, '$', n)],
                            'Reported first-launch elapsed plus rounded paper log elapsed, excluding the pause; not total elapsed.')
    manifest = paper / 'paper_manifest.json'
    for key in ('status', 'reason', 'repair_rounds_used', 'repair_round_limit', 'audit_state', 'pdf.bytes'):
        report.field(f"{name}.paper.manifest.{key}", manifest, key)
    validation = paper / 'validation_report.json'
    for key in ('ok', 'claim_count', 'citation_count', 'bibtex_key_count', 'numbers_checked', 'page_validation.total_pages', 'latex.ok'):
        report.field(f"{name}.paper.validation.{key}", validation, key)
    report.field(f"{name}.paper.saved_audit.state", paper / 'paper_evidence_audit.json', 'state')
    pdf = paper / 'paper.pdf'
    report.fact(f"{name}.paper.pdf_bytes", pdf.stat().st_size if pdf.is_file() else None, [report.source(pdf, 'stat.size')])
    # The reference validator can exit before page counting. Its retained TeX log
    # still records the actual compile output. Do not parse PDF object streams.
    log = paper / 'paper.log'
    page_lines = []
    if log.is_file():
        page_lines = [(n, re.search(r'Output written on .*?\((\d+) pages?', line)) for n, line in report.lines(log)]
        page_lines = [(n, int(m[1])) for n, m in page_lines if m]
    report.fact(f"{name}.paper.latex_log_pages", page_lines[-1][1] if page_lines else None,
                [report.source(log, '$', page_lines[-1][0] if page_lines else None)])


def summarize_ledger(report, path, selections):
    rows = report.rows(path)
    reservations, closed, settlements = {}, set(), []
    for n, e in rows:
        if e['event'] == 'reserve':
            reservations[e['id']] = (n, e)
        if e['event'] in ('release', 'settle'):
            if e['id'] not in reservations or e['id'] in closed:
                raise ValueError(f"ledger closes unknown or already closed reservation: {path}:{n}")
            closed.add(e['id'])
        if e['event'] == 'settle':
            settlements.append((n, e))
    report.fact('ledger.total_settled_usd', decimal_sum(e['usd'] for _, e in settlements), [report.source(path, 'usd', n) for n, _ in settlements])
    report.fact('ledger.open_reservations', sorted(set(reservations) - closed), [report.source(path, 'id/event')])
    for owner in sorted({e['slice'] for _, e in settlements}):
        subset = [(n, e) for n, e in settlements if e['slice'] == owner]
        report.fact(f'ledger.owner.{owner}.usd', decimal_sum(e['usd'] for _, e in subset), [report.source(path, 'usd', n) for n, _ in subset])
    for name, ids in selections.items():
        missing = set(ids) - set(reservations)
        if missing:
            raise ValueError(f"unknown ledger reservation IDs for {name}: {sorted(missing)}")
        selected = [(n, e) for n, e in settlements if e['id'] in ids]
        report.fact(f'{name}.ledger_campaign_usd', decimal_sum(e['usd'] for _, e in selected) if ids else None,
                    [report.source(path, 'usd', n) for n, _ in selected] or [report.source(path, 'id/usd')], 'Explicit reservation IDs; incomplete settlements remain open.')
        report.fact(f'{name}.ledger_phase_usd', None,
                    [report.source(path, '$', n) for n, _ in selected] or [report.source(path)],
                    'Ledger settlements cover multi-phase segments and have no independent phase-level USD field. Per-phase costs come from run events; no allocation is invented.')
        for eid in sorted(set(ids)):
            n, reservation = reservations[eid]
            settled = next(((m, e) for m, e in selected if e['id'] == eid), None)
            report.fact(f'{name}.ledger.{eid}', {
                'run': reservation['run'], 'reserved_usd': reservation['usd'], 'reserved_at': reservation['utc'],
                'settled_usd': settled[1]['usd'] if settled else None,
                'settled_at': settled[1]['utc'] if settled else None,
                'within_reservation': Decimal(settled[1]['usd']) <= Decimal(reservation['usd']) if settled else None,
                'annotation': settled[1].get('note') if settled else None,
            }, [report.source(path, '$', n)] + ([report.source(path, '$', settled[0])] if settled else []), 'Reservation/settlement timestamps bound bookkeeping, not process start/stop.')


def summarize_review(report, folder):
    path = folder / 'calls.jsonl'
    rows = report.rows(path)
    report.fact('review.calls', len(rows), [report.source(path)])
    report.fact('review.usd', decimal_sum(e['usd'] for _, e in rows), [report.source(path, 'usd', n) for n, _ in rows])
    for label in sorted({e['label'] for _, e in rows}):
        selected = [(n, e) for n, e in rows if e['label'] == label]
        report.fact(f'review.{label}.calls', len(selected), [report.source(path, 'label', n) for n, _ in selected])
        report.fact(f'review.{label}.usd', decimal_sum(e['usd'] for _, e in selected), [report.source(path, 'usd', n) for n, _ in selected])
    summary = folder / 'summary.json'
    data = report.read(summary)
    for i, paper in enumerate(data.get('papers', [])):
        report.fact(f"review.{paper['label']}.summary", paper, [report.source(summary, f'papers[{i}]')], 'Saved review; not a new reviewer call.')


def markdown(data):
    lines = ['# Campaign measurements', '', 'Generated from saved records. Decimal money and durations are JSON strings to preserve precision; unknown values are `null`.', '']
    for name, definition in data['definitions'].items():
        lines += [f'**{name}:** {definition}', '']
    # Source aliases keep the table readable while retaining exact keys and lines.
    paths = sorted(data['input_sha256'])
    aliases = {p: f'S{i}' for i, p in enumerate(paths, 1)}
    lines += ['| Measurement | Value | Evidence | Qualification |', '|---|---|---|---|']
    for f in data['measurements']:
        sources = []
        for s in f['sources']:
            text = aliases.get(s['file'], s['file']) + (f":{s['line']}" if 'line' in s else '') + '#' + s['key']
            if text not in sources:
                sources.append(text)
        cells = [f['metric'], json.dumps(f['value'], default=str, ensure_ascii=False), '; '.join(sources), f['note']]
        lines.append('| ' + ' | '.join(c.replace('|', '\\|').replace('\n', ' ') for c in cells) + ' |')
    lines += ['', '## Source files', '', '| ID | File | SHA-256 |', '|---|---|---|']
    lines += [f"| {aliases[p]} | `{p}` | `{data['input_sha256'][p]}` |" for p in paths]
    if data['warnings']:
        lines += ['', '## Warnings', ''] + [f'- {w}' for w in data['warnings']]
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--polaris', required=True, type=Path)
    parser.add_argument('--reference', required=True, type=Path)
    parser.add_argument('--ledger', required=True, type=Path)
    parser.add_argument('--json-out', required=True, type=Path)
    parser.add_argument('--markdown-out', required=True, type=Path)
    parser.add_argument('--review-dir', type=Path, help='Optional saved shared reviewer calls and summary')
    parser.add_argument('--idle-threshold-seconds', type=Decimal, default=Decimal(120))
    for name in ('polaris', 'reference'):
        parser.add_argument(f'--{name}-claims', type=Path)
        parser.add_argument(f'--{name}-rescore', type=Path, action='append', default=[])
        parser.add_argument(f'--{name}-paper-log', type=Path)
        parser.add_argument(f'--{name}-guard-log', type=Path)
        parser.add_argument(f'--{name}-ledger-id', action='append', default=[])
    args = parser.parse_args()
    if args.idle_threshold_seconds < 0:
        parser.error('idle threshold must be nonnegative')
    # Output destinations cannot overwrite any input or write inside run folders.
    outputs = [args.json_out.resolve(), args.markdown_out.resolve()]
    if outputs[0] == outputs[1]:
        parser.error('output paths must be distinct')
    for out in outputs:
        if any(out.is_relative_to(root.resolve()) for root in (args.polaris, args.reference)):
            parser.error('outputs must be outside the run folders')
    report = Report()
    try:
        for name in ('polaris', 'reference'):
            summarize_run(report, name, getattr(args, name), args.idle_threshold_seconds,
                          getattr(args, name + '_claims'), getattr(args, name + '_rescore'),
                          getattr(args, name + '_paper_log'), getattr(args, name + '_guard_log'))
        summarize_ledger(report, args.ledger, {name: getattr(args, name + '_ledger_id') for name in ('polaris', 'reference')})
        if args.review_dir:
            summarize_review(report, args.review_dir)
        if any(str(out) in report.inputs for out in outputs):
            parser.error('output would overwrite an input')
        data = {'schema_version': 1, 'definitions': DEFINITIONS, 'measurements': report.facts,
                'input_sha256': dict(sorted(report.inputs.items())), 'warnings': report.warnings}
        # Finish reading/validation before creating either output.
        for out, content in [(outputs[0], json.dumps(data, indent=2, default=str, ensure_ascii=False) + '\n'),
                             (outputs[1], markdown(data))]:
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text(content)
    except (OSError, ValueError, KeyError) as exc:
        parser.exit(1, f'report_counts: {exc}\n')


if __name__ == '__main__':
    main()
