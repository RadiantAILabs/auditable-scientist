#!/usr/bin/env python3
"""Recompute campaign measurements from the extracted records; never run saved code."""
import argparse
from collections import Counter
from decimal import Decimal
from datetime import datetime
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys
import tomllib


def module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


def rows(path):
    result = []
    for line in path.read_text(errors='replace').splitlines():
        try:
            row = json.loads(line)
            if isinstance(row, dict):
                result.append(row)
        except ValueError:
            pass
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('records', type=Path, help='Extracted scientist-records directory')
    parser.add_argument('--out', type=Path, required=True, help='Output JSON, outside the records directory')
    args = parser.parse_args()
    root = args.records.resolve()
    if args.out.resolve().is_relative_to(root):
        parser.error('output must be outside the extracted records')
    manifest = json.loads((root / 'manifest.json').read_text())
    for item in manifest['files']:
        path = root / item['path']
        if not path.resolve().is_relative_to(root) or path.is_symlink():
            raise ValueError('Unsafe manifest path')
        if hashlib.sha256(path.read_bytes()).hexdigest() != item['sha256']:
            raise ValueError('Hash mismatch: ' + item['path'])
    counting, tracing = module('report_counts'), module('action_trace')
    report = counting.Report()
    output = {'manifest_files_verified': len(manifest['files']), 'campaigns': {}}
    rates = {'input_tokens': Decimal('2'), 'cache_creation_input_tokens': Decimal('2.5'),
             'cached_input_tokens': Decimal('0.2'), 'output_tokens': Decimal('10')}
    for name in ('polaris-reported', 'polaris-first-start', 'original'):
        run = root / 'campaigns' / name
        claims = root / 'checks' / name / 'claims-v4.json'
        rescores = sorted((root / 'rescores' / name).glob('*/metrics.json'))
        counting.summarize_run(report, name, run, Decimal(120), claims,
                               [p.parent for p in rescores])
        ledger = {r['event_id']: r for r in rows(run / 'metrics/cost_events.jsonl')}
        token_totals = {k: sum(r['usage'][k] for r in ledger.values()) for k in rates}
        token_usd = sum(Decimal(v) * rates[k] / 1_000_000 for k, v in token_totals.items())
        events = rows(run / 'pipeline_events.jsonl')
        ev = tracing.Evidence()
        trace = tracing.trace_run(ev, run, 'reference' if name == 'original' else 'polaris')
        calls = [c for c in trace['calls'] if c['id'] in ledger]
        coverage, observed = 0, 0
        overwritten = []
        call_data = []
        for call in calls:
            stream = Path(call['stream']) if call['stream'] else None
            stream_rows = rows(stream) if stream else []
            responses = None
            matching = stream is not None
            if name == 'original' and stream:
                final = next((r for r in reversed(stream_rows) if r.get('type') == 'result'), None)
                expected = ledger[call['id']]['usage']
                actual = (final or {}).get('usage', {})
                matching = final is not None and all(actual.get('cache_read_input_tokens' if k == 'cached_input_tokens' else k) == expected[k] for k in rates)
                if matching:
                    responses = len({r.get('message', {}).get('id') for r in stream_rows if r.get('type') == 'assistant' and r.get('message', {}).get('id')})
                else:
                    overwritten.append(call['id'])
            elif stream:
                responses = sum(r.get('metadata', {}).get('io_type') == 'tokens' for r in stream_rows)
            if matching:
                coverage += 1
                observed += responses or 0
            actions = call['actions'] or []
            shell = [a for a in actions if a['tool'] == 'run_command']
            call_data.append({'id': call['id'], 'role': call['role'], 'phase': call['phase'],
                              'stream': str(stream.relative_to(root)) if stream else None,
                              'stream_matches_ledger': matching, 'observed_responses': responses,
                              'meter_requests': call['turns'] if name != 'original' else None,
                              'shell_commands': len(shell), 'shell_denied': sum(a['outcome'] == 'denied' for a in shell),
                              'output_tokens': ledger[call['id']]['usage']['output_tokens']})
        paper = json.loads(claims.read_text())['mechanical_check']
        review = json.loads((root / 'reviews' / name / 'summary.json').read_text())
        reviewed = next(p for p in review['papers'] if p['label'] == ('reference' if name == 'original' else 'polaris'))
        pdf = run / 'paper/paper.pdf'
        if hashlib.sha256(pdf.read_bytes()).hexdigest() != reviewed['pdf_sha256']:
            raise ValueError('Reviewed PDF differs: ' + name)
        summary = {'ledger_calls': len(ledger), 'token_totals': token_totals,
                   'token_cost_usd': str(token_usd),
                   'ledger_cost_usd': str(sum(Decimal(str(r['total_cost_usd'])) for r in ledger.values())),
                   'matching_conversations': coverage, 'stream_usage_mismatches': overwritten,
                   'observed_responses_known_subtotal': observed,
                   'models': dict(Counter(r['model'] if 'model' in r else r.get('model_id', 'unknown') for r in ledger.values())),
                   'numbers_printed': paper['numbers_printed'], 'numbers_backed': paper['numbers_backed'],
                   'review_overall': reviewed['overall'], 'review_scores': reviewed['scores'],
                   'reviewed_pdf_hash_matches': True,
                   'rescores': [json.loads(p.read_text())['combined_score'] for p in rescores],
                   'calls': call_data}
        if name != 'original':
            roles = tomllib.loads((run / 'roles.resolved.toml').read_text())['roles']
            used = {e['fields']['role'] for e in events if e['event'] == 'agent_start'}
            policies = {role: {'programs': len({p.split()[0] for p in roles[role]['shell']}),
                              'read_only': roles[role]['read_only']} for role in sorted(used)}
            summary['role_policies'] = policies
            starts = {e['fields']['step']: e for e in events if e['event'] == 'agent_start'}
            summary['agent_calls'] = [{'role': e['fields']['role'], 'step': e['fields']['step'],
                                       'requests': e['fields']['turns'], 'seconds': (datetime.fromisoformat(e['timestamp']) - datetime.fromisoformat(starts[e['fields']['step']]['timestamp'])).total_seconds()}
                                      for e in events if e['event'] == 'agent_end']
            summary['shell_commands'] = sum(c['shell_commands'] for c in call_data)
            summary['shell_denied'] = sum(c['shell_denied'] for c in call_data)
        output['campaigns'][name] = summary
    output['measurements'] = report.facts
    output['definitions'] = counting.DEFINITIONS
    output['input_sha256'] = report.inputs
    # Portable provenance: archive paths are relative to the extracted root.
    text = json.dumps(output, indent=2, default=str)
    text = text.replace(str(root) + '/', '')
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(text + '\n')
    print(json.dumps({k: {m: v for m, v in s.items() if m not in ('calls', 'agent_calls', 'role_policies')} for k, s in output['campaigns'].items()}, indent=2))


if __name__ == '__main__':
    sys.dont_write_bytecode = True
    main()
