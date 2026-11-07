#!/usr/bin/env python3
"""Reproduce fixed-prompt regression estimates from saved comparison observations.

The intercepts estimate fixed text. They are not tokenizer measurements of every
request. Original observations contain 18 distinct prompt/usage points; Polaris
uses four short, well-conditioned calls and assumes a tool-result token ratio.
"""
import argparse
import json
from pathlib import Path


def fit(xs, ys):
    mx, my = sum(xs) / len(xs), sum(ys) / len(ys)
    slope = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sum((x - mx) ** 2 for x in xs)
    intercept = my - slope * mx
    return {'points': len(xs), 'intercept_tokens': round(intercept),
            'slope_tokens_per_character': slope,
            'largest_relative_error': max(abs((intercept + slope * x - y) / y) for x, y in zip(xs, ys))}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('records', type=Path)
    args = parser.parse_args()
    folder = args.records / 'comparison/instructions'
    original = json.loads((folder / 'orig-context.json').read_text())
    points = sorted({(r['user_prompt_chars'], sum(r['attachment_chars_before_first_reply'].values()),
                      r['first_request']['total']) for r in original if r.get('session_file')})
    polaris = json.loads((folder / 'ours-first-request.json').read_text())
    good = [r for r in polaris if r['tool_result_chars_before_last_request'] <= 10000 and
            abs(r['fixed_ratio']['0.3']['from_last_request'] - r['fixed_ratio']['0.3']['from_cache_reads']) /
            r['fixed_ratio']['0.3']['from_cache_reads'] <= .07]
    print(json.dumps({'original': fit([p[0] for p in points], [p[2] for p in points]),
                      'polaris': fit([r['role_prompt_chars'] + r['task_prompt_chars'] for r in good],
                                     [r['two_equation_first_request_tokens'] for r in good])}, indent=2))


if __name__ == '__main__':
    main()
