#!/usr/bin/env python3
"""Summarise criterion results against the targets in docs/wenas-spec.md §9.

Prints a Markdown table. Always exits 0: benchmarks are goals to track, not gates.
Usage: bench-summary.py [target/criterion]
"""
import json
import sys
from pathlib import Path

# (criterion path, label, target in milliseconds)
ROWS = [
    ("first_screen/10k_lines", "First screen, 10k-line files", 30),
    ("first_screen/100k_lines", "First screen, 100k-line files", 200),
    ("hunk_copy_and_rediff_10k_lines", "Copy a hunk and re-diff, 10k lines", 16),
    ("scroll_one_frame_100k_lines", "One frame while scrolling, 100k lines", 16),
    ("directory_compare_50k_files/time_to_first_result", "Directory compare, first result (50k files)", 100),
]
EXTRA = [("directory_compare_50k_files/full_scan", "Directory compare, full scan (50k files)")]


def mean_ms(root: Path, name: str):
    f = root / name / "new" / "estimates.json"
    if not f.exists():
        return None
    return json.loads(f.read_text())["mean"]["point_estimate"] / 1e6


def fmt(ms: float) -> str:
    return f"{ms:.2f} ms" if ms < 1000 else f"{ms / 1000:.2f} s"


def main() -> None:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else "target/criterion")
    print("| Measure | Target | Measured | |")
    print("|---|---|---|---|")
    for path, label, target in ROWS:
        got = mean_ms(root, path)
        if got is None:
            print(f"| {label} | ≤ {target} ms | not run | |")
        else:
            print(f"| {label} | ≤ {target} ms | {fmt(got)} | {'✅' if got <= target else '⚠️'} |")
    for path, label in EXTRA:
        got = mean_ms(root, path)
        if got is not None:
            print(f"| {label} | streamed | {fmt(got)} | |")


if __name__ == "__main__":
    main()
