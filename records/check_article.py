#!/usr/bin/env python3
"""Check article figure data against independently recomputed saved measurements."""
import argparse
from collections import Counter
from datetime import datetime
import json
from pathlib import Path
import re
import tomllib


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('records', type=Path)
    parser.add_argument('measurements', type=Path)
    parser.add_argument('--out', type=Path)
    args = parser.parse_args()
    root = args.records
    data = json.loads(args.measurements.read_text())
    p, o = (data['campaigns'][k] for k in ('polaris-reported', 'original'))
    checks = []

    def check(label, printed, computed, article, evidence):
        checks.append({'claim': label, 'printed': printed, 'computed': computed,
                       'status': 'match' if printed == computed else 'mismatch',
                       'article': article, 'evidence': evidence})

    campaign = (root / 'article/figures/campaign.ts').read_text()
    entries = re.findall(r'\{\s*n:\s*(\d+),(.*?)\}', campaign, re.S)
    for index, ((n, entry), call) in enumerate(zip(entries, p['agent_calls'], strict=True), 1):
        i = int(n)
        time = re.search(r"time:\s*'([0-9:]+)'", entry)[1]
        turns = int(re.search(r'turns:\s*(\d+)', entry)[1])
        seconds = round(call['seconds'])
        check(f'call {i}: ordinal', i, index, 'campaign.ts', 'campaigns/polaris-reported/pipeline_events.jsonl#agent_end order')
        check(f'call {i}: wall time', time, f'{seconds // 60}:{seconds % 60:02}', 'campaign.ts', 'campaigns/polaris-reported/pipeline_events.jsonl#paired agent timestamps')
        check(f'call {i}: requests', turns, call['requests'], 'campaign.ts', 'campaigns/polaris-reported/pipeline_events.jsonl#agent_end.fields.turns')
    check('campaign call count', 33, len(p['agent_calls']), 'campaign.ts; CampaignGraph.svelte; article.svelte', 'campaigns/polaris-reported/metrics/cost_events.jsonl')
    for name, side in [('polaris-reported', p), ('original', o)]:
        for label, printed, actual in [
            ('best outside score', '2.630' if side is p else '2.636', f"{max(side['rescores']):.3f}"),
            ('overall reviewer score', 2, side['review_overall']),
            ('printed numbers', 116 if side is p else 143, side['numbers_printed']),
            ('backed numbers', 116 if side is p else 62, side['numbers_backed']),
            ('ledger calls', 33 if side is p else 56, side['ledger_calls']),
            ('full matching conversations', 33, side['matching_conversations']),
            ('token dollars', '9.01' if side is p else '9.09', f"{float(side['token_cost_usd']):.2f}")]:
            check(name + ': ' + label, printed, actual, 'ResultsTable.svelte; Grounding.svelte; Records.svelte; TimeByPhase.svelte; article.svelte',
                  f'campaigns/{name}/metrics/cost_events.jsonl; checks/{name}/claims-v4.json; reviews/{name}/summary.json; rescores/{name}/')
    for phase, pt, ot, ps, os in [('investigation',5,4,5,4),('ablations',10,17,10,0),('posthoc',3,3,3,1),('paper',15,32,15,28)]:
        for name, side, total, saved in [('polaris-reported',p,pt,ps),('original',o,ot,os)]:
            calls = [c for c in side['calls'] if c['phase'] == phase]
            check(f'{name} {phase}: calls', total, len(calls), 'Records.svelte; Topologies.svelte', f'campaigns/{name}/metrics/cost_events.jsonl; pipeline_events.jsonl')
            check(f'{name} {phase}: conversations', saved, sum(c['stream_matches_ledger'] for c in calls), 'Records.svelte', f'campaigns/{name}/ + calls[].stream in measurements.json')
    check('original missing or unusable conversations', 23, o['ledger_calls'] - o['matching_conversations'], 'article.svelte#Records caption', 'campaigns/original/metrics/cost_events.jsonl + matching stream usage')
    check('original never-saved conversations',22,sum(c['stream'] is None for c in o['calls']),'article.svelte#Records caption','campaigns/original/metrics/cost_events.jsonl + stream inventory')
    check('original overwritten conversations',1,len(o['stream_usage_mismatches']),'article.svelte#Records caption','campaigns/original/paper/ + mismatched final stream usage')
    permissions = (root / 'article/figures/Permissions.svelte').read_text()
    names = {'variant proposer':'variant_propose','variant selector':'variant_select','variant summariser':'variant_summarizer',
             'reference planner, after the draft':'reference_planner_postdraft'}
    for name, count in re.findall(r"name: '([^']+)', programs: (\d+)", permissions):
        role = names.get(name, name.replace(' ', '_').replace('post-hoc','posthoc'))
        check('named shell programs: ' + name, int(count), p['role_policies'][role]['programs'], 'Permissions.svelte', f'campaigns/polaris-reported/roles.resolved.toml#roles.{role}.shell')
    check('roles used',20,len(p['role_policies']),'Permissions.svelte; article.svelte','campaigns/polaris-reported/pipeline_events.jsonl#agent_start.fields.role')
    for label, expected, actual in [('shell attempts',244,p['shell_commands']),('shell refusals',29,p['shell_denied'])]:
        check(label,expected,actual,'article.svelte','campaigns/polaris-reported/work/**/transcript.jsonl#tool run_command')
    writer = next(c for c in p['calls'] if c['role']=='paper_writer')
    for label,expected,actual in [('writer shell attempts',73,writer['shell_commands']),('writer shell refusals',10,writer['shell_denied'])]:
        check(label,expected,actual,'article.svelte','campaigns/polaris-reported/work/paper/draft/001/transcript.jsonl')
    token_comparison = json.loads((root/'comparison/tokens.json').read_text())
    writers = {k:next(c for c in v['calls'] if c['role']=='paper_writer') for k,v in token_comparison['runs'].items()}
    for name, side in [('polaris',p),('reference',o)]:
        c=next(c for c in side['calls'] if c['role']=='paper_writer')
        check(name+' writer usage responses',67 if side is p else 28,c['observed_responses'],'article.svelte',c['stream'])
    # Average request size means all input-side tokens / usage-bearing responses.
    a,b=writers['polaris'],writers['reference']
    ratio=(a['context']['summed_input_side_tokens']/a['observed_responses'])/(b['context']['summed_input_side_tokens']/b['observed_responses'])
    check('writer average input-size ratio',1.6,round(ratio,1),'article.svelte','comparison/tokens.json#runs.*.calls[paper_writer].context')
    facts={f['metric']:f['value'] for f in data['measurements']}
    for name in ('polaris-reported','original'):
        side=p if name=='polaris-reported' else o
        check(name+' PDF pages',13,facts.get(name+'.paper.latex_log_pages'),'ResultsTable.svelte','campaigns/'+name+'/paper/ + saved LaTeX log')
    for phase,ours,original in [('investigation',8.8,10.4),('ablations',18.3,24.5),('posthoc',3.3,1.5),('paper',26.6,38.8)]:
        check('Polaris '+phase+' minutes',ours,round(float(facts['polaris-reported.closed_phase_seconds.'+phase])/60,1),'TimeByPhase.svelte','campaigns/polaris-reported/pipeline_events.jsonl#phase_end.elapsed_seconds')
        if phase!='paper':
            check('original '+phase+' minutes',original,round(float(facts['original.closed_phase_seconds.'+phase])/60,1),'TimeByPhase.svelte','campaigns/original/pipeline_events.jsonl#phase_end.elapsed')
    check('Polaris whole-run minutes',57,round(float(facts['polaris-reported.total_elapsed_seconds'])/60),'ResultsTable.svelte; TimeByPhase.svelte; article.svelte','campaigns/polaris-reported/pipeline_events.jsonl#pipeline_start/end')
    events = [json.loads(line) for line in (root/'campaigns/original/pipeline_events.jsonl').read_text().splitlines()]
    starts = [e for e in events if e['event']=='pipeline_start']
    first_end = next(e for e in events if e['event']=='pipeline_end')
    terminal = next(e for e in reversed(events) if e['event']=='paper_gate' and e['fields'].get('gate')=='finalization' and e['fields'].get('state')=='failed')
    delta=lambda a,b:(datetime.fromisoformat(b)-datetime.fromisoformat(a)).total_seconds()
    paper_seconds=delta(starts[-1]['timestamp'],terminal['timestamp'])
    active_seconds=delta(starts[0]['timestamp'],first_end['timestamp'])+paper_seconds
    check('original paper minutes',38.8,round(paper_seconds/60,1),'TimeByPhase.svelte','campaigns/original/pipeline_events.jsonl#second start to failed finalization')
    check('original active minutes',75,round(active_seconds/60),'ResultsTable.svelte; TimeByPhase.svelte; article.svelte','campaigns/original/pipeline_events.jsonl#closed first segment plus second start to finalization; shutdown unknown')
    conf=tomllib.loads((root/'campaigns/polaris-reported/config.resolved.toml').read_text())
    check('repair round limit',10,conf['paper']['max_repair_rounds'],'article.svelte','campaigns/polaris-reported/config.resolved.toml#paper.max_repair_rounds')
    check('Polaris audit calls',4,sum(c['role']=='evidence_auditor' for c in p['calls']),'article.svelte; ResultsTable.svelte; TimeByPhase.svelte','campaigns/polaris-reported/metrics/cost_events.jsonl#stage')
    check('original audit calls',11,sum(c['role']=='evidence_auditor' for c in o['calls']),'ResultsTable.svelte; TimeByPhase.svelte','campaigns/original/metrics/cost_events.jsonl#stage')
    for name,budget in [('polaris-reported',110),('original',200)]:
        manifest=json.loads((root/f'campaigns/{name}/investigation/best_program_manifest.json').read_text())
        source=(root/manifest['program_dir']/'src/program.py').read_text()
        actual=float(re.search(r'^TIME_BUDGET\s*=\s*([0-9.]+)',source,re.M)[1])
        check(name+' winner search budget',budget,actual,'TimeByPhase.svelte','campaigns/'+name+'/investigation/best_program_manifest.json -> src/program.py#TIME_BUDGET')
    variants=list((root/'comparison/earlier-variants').glob('ablation_*/task_bundle/program_1/src/program.py'))
    control=(root/'comparison/earlier-variants/control_best/task_bundle/program_0/src/program.py').read_bytes()
    check('earlier ablation intervention count',3,len(variants),'article.svelte','comparison/earlier-variants/ablation_*/task_bundle/program_1/src/program.py')
    check('earlier unchanged interventions',2,sum(p.read_bytes()==control for p in variants),'article.svelte','comparison/earlier-variants/ + byte comparison to control_best')
    for name,side in [('polaris-reported',p),('original',o)]:
        check(name+' distinct models',1,len(side['models']),'article.svelte','campaigns/'+name+'/metrics/cost_events.jsonl#model')
    for label,expected,actual in [('Polaris roles without shell',12,sum(v['programs']==0 for v in p['role_policies'].values())),('Polaris roles with shell',8,sum(v['programs']>0 for v in p['role_policies'].values()))]:
        check(label,expected,actual,'Permissions.svelte','campaigns/polaris-reported/roles.resolved.toml + pipeline_events.jsonl#roles used')
    for name in ('polaris-reported','original'):
        timeline=[json.loads(line) for line in (root/f'campaigns/{name}/pipeline_events.jsonl').read_text().splitlines()]
        first=next(e['fields'] for e in timeline if e['event']=='pipeline_start')
        workers=first['execution']['investigation']['execution']['workers'] if name=='polaris-reported' else first['execution.workers']
        check(name+' worker count',1,workers,'ResultsTable.svelte','campaigns/'+name+'/pipeline_events.jsonl#pipeline_start execution workers')
    testing=(root/'implementation/crates/polaris_scientist_run/src/runner/testing.rs').read_text()
    limit=int(re.search(r'const MAX_TESTS: u32 = (\d+);',testing)[1])
    check('advisory tests per author invocation',1,limit,'article.svelte#StepAnatomy caption','implementation/crates/polaris_scientist_run/src/runner/testing.rs#MAX_TESTS')
    fixed=json.loads((root/'comparison/instructions/ours-token-fit.json').read_text())
    check('Polaris estimated fixed prompt tokens',2500,round(fixed['intercept'],-2),'TimeByPhase.svelte; article.svelte','comparison/instructions/ours-token-fit.json; fit_prompts.py')
    fixed_text=(root/'comparison/instructions/orig-token-fit.txt').read_text()
    original_base=int(re.search(r'= (\d+) tokens',fixed_text)[1])
    check('original estimated fixed prompt tokens',15400,round(original_base,-2),'TimeByPhase.svelte; article.svelte','comparison/instructions/orig-token-fit.txt; fit_prompts.py')
    for generation,year in [('first',2024),('second',2025),('third',2026)]:
        checks.append({'claim':generation+' system publication year','printed':year,'computed':None,'status':'cannot trace',
                       'article':'Topologies.svelte','evidence':'Publication history requires the cited papers; campaign records are not publication records.'})
    article = (root/'article/article.svelte').read_text()
    if 'about a sixth of the tokens' in article:
        checks.append({'claim':'SWE-bench Verified token ratio in the separately published comparison',
                       'printed':'about one sixth', 'computed':None, 'status':'outside records',
                       'article':'article.svelte#Polaris Code introduction',
                       'evidence':'The linked polaris-code-swe-bench-tokens post reports a separate benchmark; no SWE-bench campaign is included here.'})
    result={'checks':checks,'counts':dict(Counter(c['status'] for c in checks))}
    if args.out: args.out.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result['counts'],indent=2))
    for c in checks:
        if c['status']=='outside records':
            print(f"{c['claim']}: outside these campaign records")
        elif c['status']!='match':
            print(f"{c['claim']}: printed {c['printed']}; records {c['computed']}")


if __name__=='__main__':main()
