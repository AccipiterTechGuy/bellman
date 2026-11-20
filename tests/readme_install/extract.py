#!/usr/bin/env python3
"""Turn the Linux install guide into a script for one distro, verbatim.

    extract.py GUIDE.md DISTRO PHASE [--root] [--unattended]

GUIDE.md is docs/INSTALL_LINUX.md (the whole file is the guide). An older
README.md that holds the guide under a "## Install" heading still works: then
only that section is read. Step headings may be "## " or "### ".

DISTRO is ubuntu, fedora or arch. PHASE picks what to emit:

    install    every step numbered 1. to 6.
    update     the "Update" commands, then that distro's step 4 build
               and step 5 install again, as the Update section says
    uninstall  the "Uninstall" commands (the data removal included)

A code block whose label line (the nearest non-blank line above it) names a
distro is kept only for that distro; unlabelled blocks are for everyone. Two
flags apply the changes the guide itself prescribes, and nothing else is
edited, so a failure is the guide's:

    --root        a root shell: drop `sudo`
    --unattended  nobody to answer prompts: add `--noconfirm` to pacman
"""
import re
import sys

LABELS = {"ubuntu": "debian / ubuntu", "fedora": "fedora", "arch": "arch"}


def install_section(text: str) -> str:
    """The guide: a README's exact "## Install" section, else the whole file.

    Matches the heading line exactly, so "## Install notes" or
    "## Install on macOS" never count as the start."""
    start = re.search(r"(?m)^## Install[ \t]*$", text)
    if not start:
        return text
    end = re.search(r"(?m)^## ", text[start.end():])
    return text[start.start():start.end() + end.start()] if end else text[start.start():]


def blocks(section: str):
    """Yield (heading, label, body) for every ```sh block, in order."""
    lines = section.split("\n")
    heading = ""
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.startswith(("## ", "### ")):
            heading = line.split(" ", 1)[1].strip()
        if line.strip().startswith("```sh"):
            indent = len(line) - len(line.lstrip())
            j = i - 1
            while j >= 0 and not lines[j].strip():
                j -= 1
            label = lines[j].strip().lower() if j >= 0 else ""
            body = []
            i += 1
            while not lines[i].strip().startswith("```"):
                body.append(lines[i][indent:])
                i += 1
            yield heading, label, "\n".join(body)
        i += 1


def for_distro(label: str, distro: str) -> bool:
    for d, key in LABELS.items():
        if label.startswith(key):
            return d == distro
    return True  # unlabelled: everyone


def main() -> None:
    path, distro, phase = sys.argv[1:4]
    root = "--root" in sys.argv
    unattended = "--unattended" in sys.argv
    picked = []
    for heading, label, body in blocks(install_section(open(path).read())):
        if not for_distro(label, distro):
            continue
        if phase == "install" and heading[:2] in {"1.", "2.", "3.", "4.", "5.", "6."}:
            picked.append((heading, body))
        elif phase == "update" and heading == "Update":
            picked.append((heading, body))
        elif phase == "uninstall" and heading == "Uninstall":
            picked.append((heading, body))
    if phase == "update":
        # "Then repeat your distro's command from step 4 and from step 5."
        for heading, label, body in blocks(install_section(open(path).read())):
            if heading[:2] in {"4.", "5."} and label.startswith(LABELS[distro]):
                picked.append((heading, body))
    if not picked:
        sys.exit(f"no install-guide blocks for {distro}/{phase} in {path}")
    out = ["#!/bin/bash", "set -exo pipefail", f"# README install guide: {distro} / {phase}"]
    for heading, body in picked:
        if root:
            body = re.sub(r"(?m)^(\s*)sudo ", r"\1", body)
            body = body.replace("&& sudo ", "&& ")
        if unattended:
            body = re.sub(r"(pacman -S\w*)", r"\1 --noconfirm", body)
        out += [f"\necho '=== README: {heading} ==='", body]
    print("\n".join(out))


if __name__ == "__main__":
    main()
