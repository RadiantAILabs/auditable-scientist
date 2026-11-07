#!/usr/bin/env python3
"""Deterministic publication scrub and credential scan (Python standard library)."""
import argparse
from collections import Counter
import json
from pathlib import Path
import re
import zlib
import io
import zipfile

# Only matched values are replaced; scanners report rule names, never values.
SECRET_RULES = {
    'aws_access_key': rb'\b(?:AKIA|ASIA)[A-Z0-9]{16}\b',
    'credential_assignment': rb'''(?i)((?:aws_secret_access_key|aws_session_token|secret_access_key|api[_-]?key|access[_-]?token|auth[_-]?token|password)[\\"'\s:=]+)([A-Za-z0-9_+/=.-]{24,})''',
    'bearer': rb'(?i)(Bearer\s+)[A-Za-z0-9_~+/=.-]{20,}',
    'signed_url': rb'(?i)((?:X-Amz-(?:Signature|Credential|Security-Token)|[?&](?:sig|signature|token))=)[^&\s"<>\\]+',
    'private_key': rb'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----',
    'jwt': rb'\beyJ[A-Za-z0-9_-]{12,}\.[A-Za-z0-9_-]{12,}\.[A-Za-z0-9_-]{12,}\b',
    'provider_key': rb'\b(?:sk-[A-Za-z0-9_-]{24,}|gh[pousr]_[A-Za-z0-9]{30,}|xox[baprs]-[A-Za-z0-9-]{20,})\b',
}
IDENTITY_RULES = {
    'arn': rb'arn:(?:aws|aws-us-gov|aws-cn):[^\s"\x27<>\\,]+',
    'account_id': rb'(?<![A-Za-z0-9.])\d{12}(?![A-Za-z0-9.])',
    'private_ip': rb'\b(?:10(?:\.\d{1,3}){3}|192\.168(?:\.\d{1,3}){2}|172\.(?:1[6-9]|2\d|3[01])(?:\.\d{1,3}){2}|127(?:\.\d{1,3}){3})\b',
    'cloud_host': rb'(?i)\b(?:ip-\d+(?:-\d+){3}(?:\.[a-z0-9.-]+)?|[a-z0-9.-]{1,253}\.(?:compute\.internal|ec2\.internal|compute\.amazonaws\.com))\b',
    'home': rb'/(?:work/home|home|Users)/[^/\s"\x27<>\\:]+',
    'task_path': rb'/work/tasks/[^/\s"\x27<>\\:]+',
    'local_host': rb'\b(?:[a-z][a-z0-9]{0,62}-(?:remote|dev|host)|devbox|localhost)\b',
}
PLACEHOLDERS = {k: ('<'+k.upper()+'>').encode() for k in IDENTITY_RULES}


def inspect(data):
    """Inspect bytes and decoded PDF Flate streams, without executing content."""
    parts = [data]
    if data.startswith(b'PK\x03\x04'):
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            parts.extend(archive.read(name) for name in archive.namelist())
    if data.startswith(b'%PDF'):
        for match in re.finditer(rb'stream\r?\n(.*?)\r?\nendstream', data, re.S):
            try:
                decoded = zlib.decompress(match[1])
                # PDF character-map hex strings are glyph encodings, not account IDs.
                if b'begincmap' in decoded:
                    decoded = re.sub(rb'<[0-9A-Fa-f]+>', b'<GLYPH>', decoded)
                parts.append(decoded)
            except zlib.error:
                parts.append(match[1])
    return {name: sum(len(list(re.finditer(pattern, part))) for part in parts)
            for name, pattern in {**SECRET_RULES, **IDENTITY_RULES}.items()
            if any(re.search(pattern, part) for part in parts)}


def scrub(data, replacements=()):
    counts = Counter()
    for name, pattern in SECRET_RULES.items():
        # Preserve assignment names and URL parameter names for readable records.
        def redact(match):
            return (match[1] if match.lastindex else b'') + b'<REDACTED>'
        data, n = re.subn(pattern, redact, data)
        counts[name] += n
    for before, after in sorted(replacements, key=lambda pair: -len(pair[0])):
        data, n = re.subn(re.escape(before), lambda m: after, data)
        counts['path_alias'] += n
    for name, pattern in IDENTITY_RULES.items():
        data, n = re.subn(pattern, lambda m: PLACEHOLDERS[name], data)
        counts[name] += n
    return data, +counts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    findings = []
    files = 0
    for path in sorted(args.directory.rglob('*')):
        if not path.is_file():
            continue
        files += 1
        data = path.read_bytes()
        matches = inspect(data)
        if matches:
            findings.append({'file': str(path.relative_to(args.directory)), 'rules': matches})
    print(json.dumps({'files_scanned': files, 'findings': findings}, indent=2))
    raise SystemExit(bool(findings))


if __name__ == '__main__':
    main()
