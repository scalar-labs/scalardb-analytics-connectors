#!/usr/bin/env python3
"""Print one version's section of CHANGELOG.md as its GitHub Release notes.

Usage: release-notes.py <version> <changelog>

The release job of .github/workflows/package.yml runs it. The section is the
text between the version's heading, such as `## [0.1.0] - 2026-11-24`, and the
next version's heading or the link reference definitions that end the
changelog. The heading itself is left out, since the release has its own
title.

A release body shows every line break, so a list item wrapped over several
lines in the changelog is joined into one line.

It fails when the changelog has no section for the version, or the section is
empty, so that a release is never published without notes.
"""

import re
import sys


def section_lines(changelog: str, version: str) -> list[str]:
    """Return the lines of the version's section, without its heading."""
    lines = changelog.splitlines()
    heading = f"## [{version}]"
    for start, line in enumerate(lines):
        if line.startswith(heading):
            break
    else:
        return []

    section = []
    for line in lines[start + 1 :]:
        is_next_version = line.startswith("## [")
        is_link_reference = re.match(r"\[[^\]]+\]: ", line) is not None
        if is_next_version or is_link_reference:
            break
        section.append(line)
    return section


def join_wrapped_list_items(lines: list[str]) -> list[str]:
    """Join each indented continuation line onto the list item it wraps."""
    joined: list[str] = []
    for line in lines:
        is_indented = line.startswith(" ")
        starts_nested_item = line.lstrip().startswith(("- ", "* "))
        follows_text = bool(joined) and joined[-1].strip() != ""
        if is_indented and not starts_nested_item and follows_text:
            joined[-1] += " " + line.strip()
        else:
            joined.append(line)
    return joined


def main() -> None:
    version, changelog_path = sys.argv[1], sys.argv[2]
    with open(changelog_path, encoding="utf-8") as changelog:
        lines = section_lines(changelog.read(), version)
    notes = "\n".join(join_wrapped_list_items(lines)).strip()
    if not notes:
        sys.exit(f"{changelog_path} has no section for {version}")
    print(notes)


if __name__ == "__main__":
    main()
