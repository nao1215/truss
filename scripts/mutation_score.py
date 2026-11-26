#!/usr/bin/env python3
"""Reads cargo-mutants outcomes and checks the mutation score.

The score is caught / (caught + missed), with a timed-out mutant counted as caught (a test
noticed it, by never finishing) and an unviable one left out (it did not compile, so no test
could judge it). Several output directories, such as the shards of one weekly run, are added
up before the score is taken. The run fails when the score is below --min, which is the score
recorded in docs/testing.md: the gate keeps the suite from getting weaker, it does not demand a
number the equivalent mutants make unreachable.

Usage: mutation_score.py <mutants.out dir>... [--min PERCENT] [--summary FILE]
"""

import argparse
import json
import pathlib
import sys


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("out_dirs", type=pathlib.Path, nargs="+")
    parser.add_argument("--min", type=float, default=0.0, help="lowest acceptable score in percent")
    parser.add_argument("--summary", type=pathlib.Path, help="append a Markdown summary here")
    args = parser.parse_args()

    counts = {"caught": 0, "missed": 0, "timeout": 0, "unviable": 0, "total_mutants": 0}
    missed_lines = []
    for out_dir in args.out_dirs:
        outcomes = json.loads((out_dir / "outcomes.json").read_text(encoding="utf-8"))
        for key in counts:
            counts[key] += outcomes.get(key, 0)
        missed_file = out_dir / "missed.txt"
        if missed_file.exists():
            missed_lines += missed_file.read_text(encoding="utf-8").splitlines()

    caught, missed, timeout = counts["caught"], counts["missed"], counts["timeout"]
    unviable, total = counts["unviable"], counts["total_mutants"]
    judged = caught + timeout + missed

    if judged == 0:
        line = f"mutants: {total} generated, none judged (unviable {unviable})"
        score_text = "n/a"
        ok = True
    else:
        score = 100.0 * (caught + timeout) / judged
        score_text = f"{score:.2f}%"
        line = (
            f"mutants: {total} generated, caught {caught}, timeout {timeout}, missed {missed}, "
            f"unviable {unviable}; score {score_text} (minimum {args.min:.2f}%)"
        )
        # The recorded score is the printed one, rounded to two places, so the gate compares
        # at that precision: 1,584 of 2,276 is 69.596%, recorded as 69.60, and the unrounded
        # score failed a rerun that caught exactly the same mutants.
        ok = round(score, 2) + 1e-9 >= args.min

    print(line)
    for entry in missed_lines:
        print(f"missed: {entry}")

    if args.summary:
        with args.summary.open("a", encoding="utf-8") as summary:
            summary.write(f"### Mutation score {score_text}\n\n{line}\n\n")
            if missed_lines:
                summary.write("Missed mutants:\n\n")
                for entry in missed_lines:
                    summary.write(f"- `{entry}`\n")
                summary.write("\n")

    if not ok:
        print(f"mutation score {score_text} is below the recorded {args.min:.2f}%", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
