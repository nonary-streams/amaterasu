#!/usr/bin/env python3
"""Audit source into deduplicated rejection scenarios; preserve raw evidence.

The default JSON/Markdown is a scenario report. --raw-inventory retains the old
AST inventory interface. No runtime host/plugin is contacted or policy changed.
"""
import argparse
import gzip
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

import summarize_stream_failures as scenarios


def main(argv=None):
    tool = Path(__file__).resolve().parent / "stream-failure-audit"
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--json", type=Path, required=True)
    parser.add_argument("--markdown", type=Path)
    parser.add_argument("--raw-json", type=Path, help="retained raw evidence; default OUTPUT.raw.json.gz")
    parser.add_argument("--raw-inventory", action="store_true", help="write the original raw schema-1 AST inventory instead")
    parser.add_argument("--catalog", type=Path, default=tool / "scenarios.json")
    parser.add_argument("--decision-symbol", action="append", default=[])
    parser.add_argument("--max-depth", type=int, default=5)
    parser.add_argument("--max-paths", type=int, default=3)
    parser.add_argument("--markdown-limit", type=int, default=200, help="raw-inventory Markdown limit")
    args = parser.parse_args(argv)
    command = ["cargo", "run", "--quiet", "--offline", "--locked", "--manifest-path",
               str(tool / "Cargo.toml"), "--", "--root", str(args.root),
               "--max-depth", str(args.max_depth), "--max-paths", str(args.max_paths),
               "--markdown-limit", str(args.markdown_limit)]
    for symbol in args.decision_symbol:
        command += ["--decision-symbol", symbol]
    if args.raw_inventory:
        command += ["--json", str(args.json)]
        if args.markdown:
            command += ["--markdown", str(args.markdown)]
        return subprocess.run(command, env=os.environ.copy(), check=False).returncode
    raw_path = args.raw_json or args.json.with_name(args.json.stem + ".raw.json.gz")
    outputs = [args.json, raw_path] + ([args.markdown] if args.markdown else [])
    try:
        if len({p.resolve() for p in outputs}) != len(outputs):
            raise ValueError("scenario JSON, raw evidence and Markdown outputs must differ")
        root = args.root.resolve(strict=True)
        for output in outputs:
            if output.is_symlink() or output.suffix in (".rs", ".toml", ".lock") or output.resolve() == args.catalog.resolve():
                raise ValueError("refusing source/catalog/symlink output")
            if any(output.resolve().is_relative_to(root / p) for p in ("crates", "modules", ".git")):
                raise ValueError("refusing output inside source inputs")
        # Temporary inventory is replaced by a verified compressed evidence file;
        # no source checkout, active stream, or contributor data is modified.
        with tempfile.TemporaryDirectory(prefix="stream-audit-", dir=raw_path.resolve().parent) as temp:
            raw = Path(temp) / "inventory.json"
            result = subprocess.run(command + ["--json", str(raw)], env=os.environ.copy(),
                                    capture_output=True, text=True, check=False)
            if not raw.exists():
                sys.stderr.write(result.stderr + result.stdout)
                return result.returncode or 2
            payload = raw.read_bytes()
            stored = gzip.compress(payload, mtime=0) if raw_path.suffix == ".gz" else payload
            if raw_path.suffix == ".gz" and gzip.decompress(stored) != payload:
                raise ValueError("raw compression verification failed")
            raw_path.write_bytes(stored)
        report = scenarios.load_report("source", raw_path)
        output = scenarios.summarize(json.loads(args.catalog.read_text(encoding="utf-8")), [report])
        args.json.write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")
        if args.markdown:
            args.markdown.write_text(scenarios.render(output), encoding="utf-8")
        summary = output["summary"]
        groups = output["decision_group_summary"]
        print(f"Scenario report: {summary['source_anchored_failure_or_recovery_scenarios']} source-anchored failure/recovery entries; {summary['expected_stop_scenarios']} expected stop; {summary['missing_evidence_scenarios']} unmatched/incomplete catalog entries.")
        print(f"Separate source-path index: {groups['source_confirmed_only']} confirmed cause groups; {groups['uncertain_only']} uncertain candidates. This index overlaps the reviewed entries.")
        print(f"Raw supporting evidence: {raw_path}")
        if result.returncode:
            sys.stderr.write("Partial source inventory; inspect retained errors/unmatched symbols.\n" + result.stderr)
            for error in report["raw"]["inventory"]["errors"][:8]:
                sys.stderr.write(f"Source error: {error}\n")
            for symbol in report["raw"]["unmatched_decision_symbols"]:
                sys.stderr.write(f"Unmatched decision symbol: {symbol}\n")
        return result.returncode
    except (OSError, ValueError, KeyError, IndexError, TypeError, EOFError) as error:
        print(f"stream-failure-audit: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
