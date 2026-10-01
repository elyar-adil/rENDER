"""Group the runner's compact failure list by mechanism.

The runner prints one line per non-passing (test, scripting mode) pair. This
groups those lines by the runner's mechanism name so the ranking can be read
without a terminal, and strips the scripting-mode doubling so a mechanism that
fails in both modes is reported as the number of *tests* it accounts for.
"""

import collections
import re
import sys

LINE = re.compile(
    r"^(?P<suite>\S+) (?P<file>\S+)#(?P<index>\d+) \[(?P<mode>[\w-]+)\] "
    r"(?P<outcome>.+?) :: (?P<mechanism>.*?) :: (?P<detail>.*?) :: data (?P<data>.*)$"
)


def main(path):
    by_mechanism = collections.defaultdict(list)
    by_file = collections.defaultdict(collections.Counter)
    outcome = collections.Counter()
    with open(path, encoding="utf-8-sig") as handle:
        for raw in handle:
            match = LINE.match(raw.strip())
            if not match:
                continue
            outcome[match["outcome"]] += 1
            test_id = f"{match['file']}#{match['index']}"
            by_mechanism[match["mechanism"]].append(
                (match["suite"], test_id, match["mode"], match["detail"], match["data"])
            )
            by_file[(match["suite"], match["file"])][match["outcome"]] += 1

    print("outcomes:", dict(outcome))
    print()
    rows = []
    for mechanism, entries in by_mechanism.items():
        tests = {(suite, test_id) for suite, test_id, _, _, _ in entries}
        modes = collections.Counter(mode for _, _, mode, _, _ in entries)
        rows.append((len(tests), len(entries), mechanism, modes, entries))
    rows.sort(key=lambda row: (-row[1], row[2]))
    print(f"{'cases':>6} {'tests':>6} {'on':>4} {'off':>4}  mechanism")
    for cases, tests, mechanism, modes, _ in rows:
        print(f"{cases:>6} {tests:>6} {modes['script-on']:>4} {modes['script-off']:>4}  {mechanism}")
    print()
    print("every mechanism with its distinct inputs:")
    for cases, tests, mechanism, modes, entries in rows:
        print(f"\n== {cases} cases / {tests} tests: {mechanism}   (on={modes['script-on']} off={modes['script-off']})")
        seen = set()
        for suite, test_id, mode, detail, data in entries:
            if (suite, test_id) in seen:
                continue
            seen.add((suite, test_id))
            print(f"   {test_id:>18} {data:<44} {detail}")
    print()
    print("per file:")
    for (suite, file), counts in sorted(by_file.items()):
        total = sum(counts.values())
        print(
            f"  {suite:>52} {file:<52} {total:>5} "
            + " ".join(f"{key}={value}" for key, value in sorted(counts.items()))
        )


if __name__ == "__main__":
    main(sys.argv[1])
