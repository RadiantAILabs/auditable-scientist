#!/usr/bin/env python3
"""Trace saved research-role actions without executing campaign code.

Standard library only; use python3 -I. Inputs are read only. Campaign code is never executed.
"""
from __future__ import annotations

import argparse
import ast
from collections import Counter, defaultdict
from decimal import Decimal
import hashlib
import json
from pathlib import Path
import re
import shlex


ROLE_MAP = {
    'idea_generation': 'ideation', 'attempt': 'program_author',
    'variant_edit': 'variant_author', 'variant_verify': 'variant_verifier',
    'analysis': 'posthoc_analyst', 'reference_query_plan': 'reference_planner',
    'paper_strategy': 'paper_strategist', 'paper_strategy_repair': 'paper_strategist',
    'posthoc_ranking': 'posthoc_ranker', 'paper': 'paper_writer',
    'draft_reference_gaps': 'citation_gap_analyst',
    'postdraft_reference_query_plan': 'reference_planner_postdraft',
    'postdraft_citation_weave': 'paper_enricher',
    'paper_evidence_audit': 'evidence_auditor', 'paper_repair': 'paper_repairer',
    'concept_figure': 'concept_figure',
}
ROLE_EVIDENCE = {
    'ideation': (110, 393), 'program_author': (119, 396),
    'variant_retrofit': (156, 441), 'variant_propose': (157, 442),
    'variant_select': (158, 443), 'variant_author': (159, 446),
    'variant_verifier': (159, 448), 'variant_summarizer': (163, 452),
    'posthoc_proposer': (190, 496), 'posthoc_selector': (206, 497),
    'posthoc_analyst': (210, 501), 'reference_planner': (238, 538),
    'paper_strategist': (238, 540), 'posthoc_ranker': (238, 541),
    'paper_writer': (224, 542), 'citation_gap_analyst': (225, 543),
    'reference_planner_postdraft': (225, 543), 'paper_enricher': (225, 543),
    'evidence_auditor': (246, 546), 'paper_repairer': (240, 549),
    'concept_figure': (226, 932),
}
DEFINITIONS = {
    'calls': 'One distinct top-level cost event_id per billed invocation, including zero-cost interruption. Stream-only calls are added separately; preparatory output artifacts alone do not establish a call count. Concept-figure events are distinct billed backend calls despite sharing invocation_id.',
    'retained': 'Latest started positive-duration or incomplete occurrence of each phase; zero-duration skips do not replace real work. All discarded calls remain visible. Same accounting reconstruction as report_counts.py; not a clean-run counterfactual.',
    'turns': 'Native reference result.num_turns and Polaris agent_end.fields.turns (meter requests). These native counters have different semantics. request_turns separately counts distinct reference assistant message IDs or Polaris token emissions, excluding stream fragments; it is unknown for incomplete streams.',
    'actions': 'Attempted tool invocations in stream order, never actions mentioned in prose. Compound shell tools have ordered sub-actions. Outcomes are attached from tool results; ok means tool execution succeeded, not that a program or scientific claim is correct. Unknown outcome is not success. Shell classification is heuristic and preserves unclassified other actions.',
    'request_turns_comparison': 'Stream request-turn comparisons are diagnostic because reference native num_turns differs from Polaris meter requests; native turns are the specified match dimension.',
    'sequence': 'Within each chronologically paired call, compare action kinds after collapsing adjacent repeats; preserve call boundaries. Failed and denied attempts remain in the sequence, with outcomes available separately. Missing streams yield unknown, never an empty sequence.',
    'thresholds': 'Exact call count and action sequence; native turns and output tokens within inclusive ±25%, checked per paired call and in aggregate; kept-work phase dollars within ±25% and total within ±10%. A missing value yields unknown; any known failing dimension makes the row a gap.',
    'role_attribution': 'Reference helper stages variant_edit/variant_verify are generic labels also used for retrofit, proposal, selection, README and posthoc planning. Nested stage_start/end intervals disambiguate their actual jobs, with cited event lines.',
    'money': 'Decimal native cost estimates, not invoices. Phase costs include unmapped and stream-only calls; totals with missing costs are unknown and known subtotals are supplied.',
    'paper': 'Literal TeX section headings, figure environments, graphic paths, table environments and citation keys, including local input/include files. Comments removed; macros are not executed. Figures types come from saved manifests, not image inference. PDF hash must match saved reviewer metadata. Current mechanical claims check is separate from saved model audit.',
}


def native_role(stage):
    return re.sub(r'_round_\d+$', '', stage)


def mapped_role(stage):
    return ROLE_MAP.get(native_role(stage), native_role(stage))


def phase_for(role, fallback='unknown'):
    if role in ('ideation', 'program_author'): return 'investigation'
    if role.startswith('variant_'): return 'ablations'
    if role in ('posthoc_proposer', 'posthoc_selector', 'posthoc_analyst'): return 'posthoc'
    if role in ROLE_EVIDENCE: return 'paper'
    return fallback


class Evidence:
    def __init__(self):
        self.inputs = {}
        self.warnings = []

    def source(self, path, line=1, key='$'):
        path = Path(path).resolve()
        if str(path) not in self.inputs:
            if path.is_file():
                self.inputs[str(path)] = hashlib.sha256(path.read_bytes()).hexdigest()
            else:
                self.inputs[str(path)] = None
        return {'file': str(path), 'line': line, 'key': key, **({'missing': True} if not path.is_file() else {})}

    def rows(self, path):
        self.source(path)
        if not path.is_file(): return []
        rows = []
        lines = path.read_text(errors='replace').splitlines()
        for n, line in enumerate(lines, 1):
            if not line.strip() or line.startswith('[container_agent]'): continue
            if not line.lstrip().startswith(('{', '[')): continue
            try:
                row = json.loads(line)
                if isinstance(row, dict):rows.append((n, row))
                else:self.warnings.append(f'{path}:{n}: non-object record ignored')
            except json.JSONDecodeError:
                self.warnings.append(f'{path}:{n}: non-JSON or truncated record ignored')
        return rows

    def fact(self, value, path, line=1, key='$'):
        return {'value': value, 'sources': [self.source(path, line, key)]}

    def json(self, path):
        self.source(path)
        return json.loads(path.read_text()) if path.is_file() else None


def write_kind(path, patch=False):
    path = str(path).lower()
    if re.search(r'(?:rubric_status|manifest|claims_grounding|figure_manifest|.*status)\.(?:json|toml)', path):
        return 'write_status_manifest'
    if re.search(r'memory|conclusions', path): return 'write_memory'
    if re.search(r'(?:program[^/]*|/src)/|(?:^|/)program\.py', path):
        return 'patch_program' if patch else 'write_program'
    if re.search(r'\.tex\b', path): return 'write_manuscript'
    return 'write_file'


def python_actions(code):
    """Inspect literal Python syntax as data; never evaluate it."""
    try:
        tree = ast.parse(code)
    except SyntaxError:
        return []
    result = []
    calls = sorted((node for node in ast.walk(tree) if isinstance(node, ast.Call)),
                   key=lambda node: (node.lineno, node.col_offset))
    for node in calls:
        name = ast.unparse(node.func)
        if name == 'open' and node.args and isinstance(node.args[0], ast.Constant):
            mode = node.args[1].value if len(node.args) > 1 and isinstance(node.args[1], ast.Constant) else ''
            mode = next((kw.value.value for kw in node.keywords if kw.arg == 'mode' and isinstance(kw.value, ast.Constant)), mode)
            if any(c in str(mode) for c in 'wa+'):
                result.append(write_kind(node.args[0].value, patch='r+' in str(mode)))
        elif name.endswith(('.write_text', '.write_bytes')) and isinstance(node.func, ast.Attribute):
            receiver = node.func.value
            if isinstance(receiver, ast.Call) and receiver.args and isinstance(receiver.args[0], ast.Constant):
                result.append(write_kind(receiver.args[0].value))
    return result


def shell_actions(command):
    # Extract heredoc bodies before shell tokenization so source code and quoted
    # examples cannot masquerade as commands that were actually run.
    bodies = []
    def heredoc(match):
        bodies.append(match[2])
        return match[0].split('\n', 1)[0].split('<<', 1)[0] + '\n'
    command = re.sub(r"<<-?\s*['\"]?(\w+)['\"]?[^\n]*\n(.*?)\n\1\b", heredoc, command, flags=re.S)
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=';&|<>\n')
        lexer.whitespace = ' \t\r'
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError: return ['other']
    groups, group = [], []
    for token in tokens + [';']:
        if token and all(c in ';&|\n' for c in token):
            if group: groups.append(group); group = []
        else: group.append(token)
    kinds = []
    cwd = ''
    for args in groups:
        while args and (args[0] in ('env', 'timeout', 'command') or re.match(r'^\w+=', args[0]) or re.match(r'^\d+(?:s|m)?$', args[0])): args = args[1:]
        if not args: continue
        exe = Path(args[0]).name
        text = ' '.join(args)
        if exe == 'cd':
            cwd = args[1] if len(args)>1 else ''; continue
        if exe in ('export', 'set'): continue
        if exe in ('ls', 'find', 'pwd', 'tree'): kind = 'list_files'
        elif exe in ('cat', 'head', 'tail', 'less'): kind = 'read_file'
        elif exe in ('rg', 'grep', 'awk', 'wc', 'diff'): kind = 'read_file'
        elif exe == 'sed': kind = write_kind(args[-1], True) if any(x.startswith('-i') for x in args[1:]) else 'read_file'
        elif exe in ('cp', 'rsync'):
            kind = 'clone_program' if re.search(r'program[_/\d]|reference_program', text) else 'copy_file'
        elif exe == 'git' and 'clone' in args: kind = 'clone_program'
        elif exe == 'mv' and '/program_' in cwd: kind='write_program'
        elif exe in ('mkdir', 'chmod', 'mv', 'rm', 'touch'): kind = 'prepare_workspace'
        elif exe in ('pdflatex', 'latexmk', 'xelatex', 'lualatex', 'bibtex'): kind = 'compile_latex'
        elif exe in ('curl', 'wget'):
            kind = 'literature_search' if re.search(r'openalex|semantic|arxiv|scholar|crossref', text, re.I) else 'fetch_web'
        elif exe.startswith('python') or 'TASK_PYTHON' in exe or 'WRAPPER_PYTHON' in exe:
            if re.search(r'(?:evaluate_program|(?:^|[/ ])evaluate\.py|check-claims)', text): kind = 'run_evaluator'
            elif 'clone_program' in text or 'program_clone' in text: kind = 'clone_program'
            else:
                code = args[args.index('-c')+1] if '-c' in args and args.index('-c')+1 < len(args) else ''
                derived = python_actions(code or (bodies.pop(0) if bodies else ''))
                if '/program_' in cwd and any(k=='write_file' for k in derived):
                    derived = ['patch_program' if k=='write_file' else k for k in derived]
                if derived: kinds.extend(derived); continue
                kind = 'run_program'
        elif exe in ('pytest', 'cargo', 'make'): kind = 'run_program'
        elif exe in ('echo', 'printf', 'tee'): kind = 'write_file' if '>' in text or exe == 'tee' else 'other'
        else: kind = 'other'
        # Shell writes are classified by destination, not text inside the file.
        if any(a in ('>', '>>') for a in args):
            at = next(i for i, a in enumerate(args) if a in ('>', '>>'))
            if at + 1 < len(args) and exe in ('cat', 'echo', 'printf', 'tee'):
                kind = write_kind(str(Path(cwd)/args[at+1]))
        kinds.append(kind)
    return kinds or ['other']


def classify(tool, args):
    tool = tool.lower()
    if isinstance(args, str): path = args
    else: path = args.get('file_path', args.get('path', args.get('pattern', '')))
    if tool in ('bash', 'run_command', 'run_in_background'):
        return shell_actions(args if isinstance(args, str) else args.get('command', ''))
    if tool == 'test_candidate': return ['run_evaluator']
    if tool in ('read', 'read_file', 'read_span', 'search', 'grep'): return ['read_file']
    if tool in ('glob', 'list_files', 'ls'): return ['list_files']
    if tool in ('write', 'create_file'): return [write_kind(path)]
    if tool in ('edit', 'edit_file', 'notebookedit'): return [write_kind(path, True)]
    if tool in ('websearch', 'web_search'): return ['literature_search']
    if tool in ('webfetch', 'web_fetch'): return ['fetch_web']
    if tool == 'skill': return ['load_skill']
    if tool in ('taskoutput', 'task_output'): return ['read_process_output']
    if tool in ('task', 'agent'): return ['delegate']
    if tool.startswith('python'): return ['run_program']
    return ['other']


def parse_stream(ev, path, system):
    rows = ev.rows(path)
    result = {'actions': None, 'turns': None, 'request_turns': None, 'tokens': {}, 'usd': None, 'sources': {}}
    if not rows: return result
    actions, pending, messages, token_lines = [], {}, set(), []
    observed_tokens = {'input_tokens':0,'output_tokens':0}
    complete = False
    final = None
    for n, row in rows:
        if system == 'reference':
            if row.get('type') == 'result': final = (n, row)
            message = row.get('message', {})
            if row.get('type') == 'assistant':
                if message.get('id'): messages.add(message['id'])
                for block in message.get('content', []):
                    if not isinstance(block, dict) or block.get('type') != 'tool_use': continue
                    ident = block['id']
                    if ident in pending: continue
                    action = {'tool': block['name'], 'kinds': classify(block['name'], block.get('input', {})), 'outcome_scope':'whole_tool',
                              'outcome': 'unknown', 'sources': [ev.source(path, n, 'message.content.tool_use')]}
                    actions.append(action); pending[ident] = action
            if row.get('type') == 'user':
                content = message.get('content', [])
                for block in content if isinstance(content, list) else []:
                    if not isinstance(block, dict): continue
                    if block.get('tool_use_id') in pending:
                        action = pending[block['tool_use_id']]
                        text = str(block.get('content', ''))
                        action['outcome'] = 'error' if block.get('is_error') else 'ok'
                        if re.search(r'^(?:Execution error: )?Permission denied|^Permission denied:|^Error:.*not allowed', text, re.I): action['outcome'] = 'denied'
                        action['sources'].append(ev.source(path, n, 'message.content.tool_result'))
        else:
            meta = row.get('metadata', {})
            if meta.get('final') == 'true': complete = meta.get('outcome') == 'complete'
            if meta.get('io_type') == 'tokens':
                token_lines.append(n)
                for key,short in [('input_tokens','in'),('output_tokens','out')]:
                    observed_tokens[key] += int(meta.get(short,0))
            if meta.get('io_type') == 'status':
                try:
                    status=json.loads(meta.get('status_report','{}'))
                    if 'cost_usd' in status:
                        result['usd']=status['cost_usd'];result['sources']['usd']=[ev.source(path,n,'metadata.status_report.cost_usd')]
                except (ValueError,TypeError):pass
            if meta.get('io_type') != 'tool': continue
            ident = meta.get('id', str(n))
            if meta.get('phase') == 'start':
                if ident in pending: continue
                text = row.get('content', {}).get('Text', '')
                action = {'tool': meta.get('label', 'unknown'), 'kinds': classify(meta.get('label', ''), text),
                          'outcome': 'unknown', 'outcome_scope':'whole_tool', 'sources': [ev.source(path, n, 'metadata/content')]}
                actions.append(action); pending[ident] = action
            elif ident in pending:
                action = pending[ident]
                action['outcome'] = meta.get('phase', 'unknown')
                if re.search(r'^(?:Execution error: )?Permission denied|^Permission denied:', row.get('content',{}).get('Text',''), re.I):
                    action['outcome']='denied'
                if action['tool']=='test_candidate':
                    try:
                        payload=json.loads(row.get('content',{}).get('Text','{}'))
                        if isinstance(payload,dict) and payload.get('error'):action['outcome']='error'
                    except ValueError:pass
                document = meta.get('document')
                if document:
                    try:
                        doc = json.loads(document)
                        if doc.get('exit_code') not in (None, 0) or doc.get('timed_out'): action['outcome'] = 'error'
                    except (ValueError, TypeError): pass
                action['sources'].append(ev.source(path, n, 'metadata.phase/document'))
    result['actions'] = actions
    if system == 'reference' and final:
        n, row = final
        result.update(turns=row.get('num_turns'), request_turns=len(messages) if messages else None,
                      tokens=row.get('usage', {}), usd=row.get('total_cost_usd'))
        for key in ('turns', 'tokens', 'usd'): result['sources'][key] = [ev.source(path, n, {'turns':'num_turns','tokens':'usage','usd':'total_cost_usd'}[key])]
        result['sources']['request_turns'] = [ev.source(path, n, 'distinct assistant.message.id') for n, r in rows if r.get('type') == 'assistant']
    elif system == 'polaris':
        result['request_turns'] = len(token_lines) if token_lines and complete else None
        result['tokens'] = observed_tokens if complete else {}
        result['sources']['tokens'] = [ev.source(path,n,'metadata.in/out') for n in token_lines]
        result['sources']['request_turns'] = [ev.source(path, n, 'metadata.io_type=tokens') for n in token_lines]
    return result


def phase_occurrences(ev, root):
    path = root/'pipeline_events.jsonl'
    rows = ev.rows(path)
    starts, active, keep = [], {}, {}
    for n, row in rows:
        fields = row.get('fields', {})
        if row.get('event') == 'pipeline_start': active = {}
        if row.get('event') == 'phase_start':
            phase = fields.get('phase_name', row.get('phase', 'unknown'))
            entry = {'phase': phase, 'timestamp': row['timestamp'], 'line': n}
            starts.append(entry); active[phase] = entry; keep[phase] = entry
        if row.get('event') == 'phase_end':
            phase = fields.get('phase_name', row.get('phase', 'unknown'))
            if phase not in active: continue
            entry = active.pop(phase)
            if fields.get('elapsed_seconds') == 0 or fields.get('elapsed') in ('0s', '0.0s'):
                previous = [x for x in starts if x['phase']==phase and x['line']<entry['line']]
                if previous: keep[phase] = previous[-1]
                else: keep.pop(phase, None)
    return rows, starts, keep


def reference_stage_roles(ev, root):
    intervals=[]
    roles={'ablations':{1:'variant_retrofit',2:'variant_propose',3:'variant_select',8:'variant_summarizer'},
           'posthoc':{2:'posthoc_proposer',3:'posthoc_selector'}}
    for phase in roles:
        path=root/phase/'events.jsonl'
        active={}
        for n,row in ev.rows(path):
            fields=row.get('fields',{});stage=fields.get('stage')
            if row.get('event')=='stage_start': active[stage]=(n,row['timestamp'])
            if row.get('event')=='stage_end' and stage in active:
                start_line,start=active.pop(stage)
                if stage in roles[phase]:
                    intervals.append((start,row['timestamp'],roles[phase][stage],phase,[ev.source(path,start_line,'fields.stage'),ev.source(path,n,'fields.stage')]))
    return intervals


def discover_streams(root, system):
    if system == 'polaris':
        return sorted(set((root/'work').rglob('transcript.jsonl')) | set((root/'_reset').glob('*/work/**/transcript.jsonl')) | ({root/'transcript.jsonl'} if (root/'transcript.jsonl').is_file() else set()))
    # Include prefixed idea/posthoc/paper logs as well as plain author logs.
    return sorted(set(root.rglob('*agent_claude*.log')) | set(root.rglob('agent_claude.jsonl')))


def trace_run(ev, root, system, phase_filter=None, role_override=None):
    root = root.resolve()
    events, starts, keep = phase_occurrences(ev, root)
    ep, cp = root/'pipeline_events.jsonl', root/'metrics/cost_events.jsonl'
    ledger = ev.rows(cp)
    streams = discover_streams(root, system)
    intervals = reference_stage_roles(ev,root) if system=='reference' else []
    used, seen, calls = set(), {}, []
    for n, row in ledger:
        eid = row['event_id']
        if eid in seen:
            if seen[eid] != row: raise ValueError(f'conflicting cost event_id {eid}')
            continue
        seen[eid] = row
        role = mapped_role(row.get('stage', 'unknown'))
        role_sources=[]
        for begin,end,stage_role,stage_phase,sources in intervals:
            if begin <= row['timestamp'] <= end and row.get('stage') in ('variant_edit','variant_verify'):
                role=stage_role;role_sources=sources;break
        before = [s for s in starts if s['timestamp'] <= row['timestamp']]
        start = before[-1] if before else None
        phase = start['phase'] if start else phase_for(role, row.get('phase', 'unknown'))
        retained = start == keep.get(phase) if start else True
        if phase_filter and phase not in phase_filter: continue
        artifact = root/row.get('artifact', '__missing__')
        path = artifact/'transcript.jsonl' if system == 'polaris' else artifact
        stream = path if path.is_file() and path in streams else None
        if stream is None and system=='polaris':
            candidates=[p for p in streams if str(p.parent).endswith('/'+row.get('artifact','___')) and p not in used]
            if len(candidates)==1:stream=candidates[0]
        # Old variant ledgers may have lost directory prefixes. Only use a unique
        # basename candidate; never assign an arbitrary log to several calls.
        if stream is None and system == 'reference':
            candidates = [p for p in streams if p.name == path.name and p not in used]
            if len(candidates) == 1: stream = candidates[0]
        if stream in used: stream = None
        if stream: used.add(stream)
        parsed = parse_stream(ev, stream, system) if stream else {'actions': None, 'turns': None, 'request_turns': None, 'sources': {}}
        src = ev.source(cp, n)
        call = {'id': eid, 'system': system, 'role': role, 'native_role': row.get('stage'), 'phase': phase,
                'retained': retained, 'timestamp': row['timestamp'], 'status': row.get('status'),
                'turns': parsed['turns'], 'request_turns': parsed['request_turns'],
                'tokens': {k: row.get('usage', {}).get(v) for k,v in {'input':'input_tokens','cache_read':'cached_input_tokens','cache_write':'cache_creation_input_tokens','output':'output_tokens'}.items()},
                'usd': str(row['total_cost_usd']) if row.get('total_cost_usd') is not None else None,
                'actions': parsed['actions'], 'stream': str(stream) if stream else None,
                'sources': {k:[{**src,'key':v}] for k,v in {'role':'stage','native_role':'stage','timestamp':'timestamp','status':'status','tokens':'usage','usd':'total_cost_usd','id':'event_id'}.items()}}
        # Cost and token totals come from the exact Decimal ledger fields.
        call['sources'].update({k:v for k,v in parsed['sources'].items() if k not in ('usd','tokens')})
        call['sources']['role'] += role_sources
        call['sources'].setdefault('turns',[ev.source(stream or path,1,'num_turns')])
        call['sources'].setdefault('request_turns',[ev.source(stream or path,1,'assistant IDs / token emissions')])
        call['sources']['phase'] = [ev.source(ep, start['line'], 'fields.phase_name')] if start else [src]
        call['sources']['retained'] = call['sources']['phase'] + ([ev.source(ep, keep[phase]['line'], 'latest phase occurrence')] if phase in keep else [])
        if system == 'polaris':
            ends = [(m,e) for m,e in events if e.get('event')=='agent_end' and e.get('fields',{}).get('workspace','').rstrip('/').endswith(row.get('artifact','___')) and e.get('fields',{}).get('step')==row.get('invocation_id')]
            if len(ends)==1:
                m,e = ends[0];call['turns'] = e['fields'].get('turns');call['sources']['turns']=[ev.source(ep,m,'fields.turns')]
            if call['status'] not in ('completed', 'complete'): call['request_turns'] = None
        if not stream: ev.warnings.append(f'{system} {eid}: missing tool stream for {row.get("artifact")}; actions unknown, turns require an independent event counter')
        calls.append(call)
    # Partial/replay invocations can have a stream before a ledger settles.
    for path in streams:
        if path in used: continue
        parsed = parse_stream(ev,path,system)
        if system == 'polaris':
            start = next(((n,e) for n,e in events if e.get('event')=='agent_start' and e.get('fields',{}).get('workspace','').rstrip('/').endswith(str(path.parent.relative_to(root)))), None)
            role = start[1]['fields']['role'] if start else (role_override or 'unknown')
            phase = start[1].get('phase', phase_for(role)) if start else phase_for(role)
        else:
            stage = re.sub(r'_agent_claude.*', '', path.stem)
            role = mapped_role('attempt' if stage=='agent_claude' or path.name.startswith('agent_claude_') else ('idea_generation' if stage=='idea' else 'analysis' if stage=='posthoc' else stage))
            phase = phase_for(role)
        if phase_filter and phase not in phase_filter: continue
        src = ev.source(path)
        calls.append({'id':'stream:'+str(path.relative_to(root)), 'system':system,'role':role,'native_role':role,'phase':phase,'retained':True,'timestamp':None,'status':'unsettled_or_unledgered', 'role_origin':'cli_override' if role_override else 'stream_or_event',
                      'turns':parsed['turns'],'request_turns':parsed['request_turns'],
                      'tokens':{k:parsed.get('tokens',{}).get(v) for k,v in {'input':'input_tokens','cache_read':'cache_read_input_tokens','cache_write':'cache_creation_input_tokens','output':'output_tokens'}.items()},
                      'usd':str(parsed['usd']) if parsed.get('usd') is not None else None, 'actions':parsed['actions'],'stream':str(path),
                      'sources':{**{k:[src] for k in ('id','role','phase','native_role','retained','status','timestamp','turns','request_turns','tokens','usd')},**parsed['sources']}})
    calls.sort(key=lambda c:(c['timestamp'] is None,c['timestamp'] or c['id']))
    return {'root':str(root),'calls':calls,'ledger_events':len(seen),'stream_inventory':[ev.source(p) for p in streams],
            'phase_occurrences':[{**s,'retained':keep.get(s['phase'])==s,'source':ev.source(ep,s['line'])} for s in starts]}

