#!/usr/bin/env python3
"""Mechanical originality sweep: Bellman sources vs the inspiration clones.

Usage:  BELLMAN_REFERENCE_REPOS=/path/to/clones python3 docs/qa-c11/originality_sweep.py
Defaults: repo root inferred from this file; clones at ~/reference_repos/bellman.

Three independent probes, each reported per Bellman module:
  1. shingle    — normalised 6-line code shingles shared with any reference repo
  2. identifier — declared names (fn/struct/enum/trait/const) shared with a repo
  3. literal    — string literals >= 12 chars appearing verbatim in a repo
  4. comment    — comment lines >= 40 chars appearing verbatim in a repo
"""
import hashlib
import os
import json
import re
import sys
from pathlib import Path
from collections import defaultdict

OURS_ROOT = Path(os.environ.get("BELLMAN_ROOT",
                               Path(__file__).resolve().parents[2]))
REFS_ROOT = Path(os.environ.get("BELLMAN_REFERENCE_REPOS",
                               Path.home() / "reference_repos" / "bellman"))

CODE_EXT = {".rs", ".js", ".ts", ".mjs", ".cpp", ".h", ".hpp", ".cc", ".py", ".svelte"}

DECL_RE = re.compile(
    r"\b(?:fn|struct|enum|trait|const|static|type)\s+([A-Za-z_][A-Za-z0-9_]*)")
STR_RE = re.compile(r'"((?:[^"\\]|\\.){12,})"')
LINE_COMMENT_RE = re.compile(r"^\s*(?://|#|\*|/\*)\s?(.*)$")

# Names too generic to mean anything.
GENERIC = set("""
new next now run len is_empty from into default clone fmt drop main test tests
next_fire fire timer timers scheduler schedule config Config Error error Result
start stop poll push pop id name path time date state kind status value
open close read write send recv get set add remove delete update list
""".split())


def norm_code_lines(text):
    out = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line:
            continue
        if line.startswith(("//", "#", "*", "/*", "*/")):
            continue
        line = re.sub(r"\s+", " ", line)
        out.append(line)
    return out


def shingles(lines, n=6):
    for i in range(len(lines) - n + 1):
        blob = "\n".join(lines[i:i + n])
        yield hashlib.sha1(blob.encode()).hexdigest()[:16], i + 1


def collect_files(root):
    for p in root.rglob("*"):
        if not p.is_file() or p.suffix not in CODE_EXT:
            continue
        if any(part in {".git", "node_modules", "target", "dist", "build",
                        "vendor"} for part in p.relative_to(root).parts):
            continue
        yield p


def module_of(p):
    rel = p.relative_to(OURS_ROOT)
    parts = rel.parts
    if parts[0] == "crates":
        # crates/<crate>/src/<module>/...
        if len(parts) >= 4 and parts[2] in ("src", "tests", "examples"):
            if parts[2] != "src":
                return f"{parts[1]}/{parts[2]}"
            if len(parts) >= 5:
                return f"{parts[1]}/{parts[3]}"
            return f"{parts[1]}/{parts[3] if len(parts) > 3 else 'src'}"
        return parts[1]
    if parts[0] == "src-tauri":
        return "bellman-app (src-tauri)"
    if parts[0] == "ui":
        return "ui (Svelte)"
    if parts[0] == "testing_apps":
        return f"testing_apps/{parts[1]}"
    if parts[0] == "helpers":
        return "helpers"
    return parts[0]


def main():
    # ---- index the references -------------------------------------------
    ref_shingle = {}          # hash -> "repo:file:line"
    ref_decl = defaultdict(set)   # name -> {repo}
    ref_text = {}             # repo -> concatenated text (for literal search)
    for repo_dir in sorted(REFS_ROOT.iterdir()):
        if not repo_dir.is_dir():
            continue
        repo = repo_dir.name
        chunks = []
        for f in collect_files(repo_dir):
            try:
                text = f.read_text(errors="replace")
            except OSError:
                continue
            chunks.append(text)
            lines = norm_code_lines(text)
            for h, ln in shingles(lines):
                ref_shingle.setdefault(h, f"{repo}:{f.relative_to(repo_dir)}:{ln}")
            for m in DECL_RE.finditer(text):
                ref_decl[m.group(1)].add(repo)
        ref_text[repo] = "\n".join(chunks)
        print(f"indexed {repo}: {len(ref_text[repo])} chars", file=sys.stderr)

    # ---- scan ours -------------------------------------------------------
    findings = defaultdict(lambda: {"files": 0, "lines": 0, "shingle": [],
                                    "identifier": [], "literal": [], "comment": []})
    ours_roots = [OURS_ROOT / "crates", OURS_ROOT / "src-tauri" / "src",
                  OURS_ROOT / "ui" / "src", OURS_ROOT / "testing_apps",
                  OURS_ROOT / "helpers"]
    for root in ours_roots:
        if not root.exists():
            continue
        for f in collect_files(root):
            try:
                text = f.read_text(errors="replace")
            except OSError:
                continue
            mod = module_of(f)
            rec = findings[mod]
            rec["files"] += 1
            lines = norm_code_lines(text)
            rec["lines"] += len(lines)
            rel = str(f.relative_to(OURS_ROOT))
            for h, ln in shingles(lines):
                if h in ref_shingle:
                    rec["shingle"].append(f"{rel}:{ln} == {ref_shingle[h]}")
            for m in DECL_RE.finditer(text):
                nm = m.group(1)
                if nm in GENERIC or len(nm) < 6:
                    continue
                if nm in ref_decl:
                    rec["identifier"].append(f"{nm} (also in {sorted(ref_decl[nm])})")
            for m in STR_RE.finditer(text):
                lit = m.group(1)
                if len(lit) < 16 or lit.count(" ") < 1:
                    continue
                for repo, blob in ref_text.items():
                    if lit in blob:
                        rec["literal"].append(f"{rel}: {lit!r} also in {repo}")
                        break
            for raw in text.splitlines():
                cm = LINE_COMMENT_RE.match(raw)
                if not cm:
                    continue
                c = cm.group(1).strip()
                if len(c) < 40:
                    continue
                for repo, blob in ref_text.items():
                    if c in blob:
                        rec["comment"].append(f"{rel}: {c!r} also in {repo}")
                        break

    out = {}
    for mod, rec in sorted(findings.items()):
        rec["identifier"] = sorted(set(rec["identifier"]))
        out[mod] = rec
    print(json.dumps(out, indent=2))


if __name__ == "__main__":
    main()
