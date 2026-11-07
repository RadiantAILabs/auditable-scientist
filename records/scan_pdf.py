#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["pypdf==6.1.1"]
# ///
"""Scan decoded PDF text and metadata without printing matched values."""
import argparse
import importlib.util
import json
from pathlib import Path
from pypdf import PdfReader


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location('publication_scrub', Path(__file__).with_name('scrub.py'))
    scrub = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(scrub)
    findings = []
    count = pages = 0
    for path in sorted(args.directory.rglob('*.pdf')):
        reader = PdfReader(path)
        chunks = [str(reader.metadata or {})]
        for page in reader.pages:
            chunks.append(page.extract_text())
            pages += 1
        count += 1
        matches = scrub.inspect('\n'.join(chunks).encode())
        if matches:
            findings.append({'file': str(path.relative_to(args.directory)), 'rules': matches})
    print(json.dumps({'pdfs_scanned': count, 'pages_scanned': pages, 'findings': findings}, indent=2))
    raise SystemExit(bool(findings))


if __name__ == '__main__':
    main()
