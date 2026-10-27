#!/usr/bin/env python3
"""Withdraw one approved web extension from a stopped development Linux root."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import stat
import sys
import tempfile

from setup_linux_management import private_directory, regular_file, sync_directory


def valid_id(value):
    return (isinstance(value, str) and 0 < len(value) <= 64
            and not value.startswith('-') and not value.endswith('-')
            and all('a' <= char <= 'z' or '0' <= char <= '9' or char == '-' for char in value))


def withdraw(root, extension_id):
    if not root.is_absolute() or root.resolve() != root:
        raise ValueError('root must be an absolute, normalized path without symlinks')
    private_directory(root)
    if not valid_id(extension_id):
        raise ValueError('invalid extension id')
    lock_path = root / 'host.lock'
    fd = os.open(lock_path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'r+') as lock:
        regular_file(lock_path)
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        path = root / 'bootstrap.json'
        regular_file(path, True)
        bootstrap = json.loads(path.read_text())
        if bootstrap.get('development') is not True:
            raise ValueError('only a development Linux session root is supported')
        settings = bootstrap['policy']['settings']
        extensions = settings['amaterasu.management']['extensions']
        entries = extensions['entries']
        if not isinstance(entries, list):
            raise ValueError('extension entries must be an array')
        matches = []
        ids = set()
        for index, entry in enumerate(entries):
            if not isinstance(entry, dict) or not valid_id(entry.get('id')):
                raise ValueError('each extension approval needs a valid explicit id; upgrade older policy first')
            entry_id = entry['id']
            if entry_id in ids:
                raise ValueError('duplicate extension id in policy')
            ids.add(entry_id)
            if entry_id == extension_id:
                matches.append(index)
        if len(matches) != 1:
            raise ValueError('extension id must match exactly one approved entry')
        auth = settings['amaterasu.authentication']
        generation = auth['authorization_policy_generation']
        if type(generation) is not int or not 0 < generation < 2**64 - 1:
            raise ValueError('invalid or exhausted authorization generation')
        del entries[matches[0]]
        auth['authorization_policy_generation'] = generation + 1
        fd, temporary = tempfile.mkstemp(prefix='.bootstrap-', dir=root)
        try:
            with os.fdopen(fd, 'w') as output:
                os.fchmod(output.fileno(), stat.S_IMODE(path.stat().st_mode))
                json.dump(bootstrap, output, indent=2)
                output.write('\n')
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, path)
            sync_directory(root)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
    return generation + 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('extension_id')
    args = parser.parse_args()
    try:
        generation = withdraw(args.root, args.extension_id)
        print(f'Extension withdrawn; authorization generation is {generation}. Restart the host to apply. Close all previously open browser tabs: extension code already loaded in a tab can still run.')
    except (ValueError, OSError, KeyError, TypeError, json.JSONDecodeError) as error:
        print(f'withdrawal failed: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
