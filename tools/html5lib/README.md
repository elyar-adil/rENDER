# html5lib tree-construction conformance

rENDER's HTML parser measured against the reference tree-construction suite.

**The measurement is the deliverable.** The runner is only the instrument, and it
reports four separate outcomes rather than one percentage, because a bare
`pass / total` counts a test it could not even attempt the same as a test whose
tree is wrong, and those are not the same claim about the engine.

| outcome | meaning |
| --- | --- |
| **pass** | the parsed tree equals the suite's expected tree |
| **engine defect** | the trees differ in a way that matters |
| **acceptable difference** | they differ and it does not matter; every such rule prints its reason |
| **unimplemented feature** | the test exercises something this engine does not do, and the test says so |
| **harness limitation** | the runner could not evaluate it |

The classification is a rule table, not a heuristic, and its fall-through is
**engine defect**. A runner that invents excuses for what it does not understand
is how a project ends up reporting a number it cannot defend.

## Fetching the suite (pinned, never committed)

```powershell
powershell -File tools/html5lib/fetch-html5lib.ps1
```

The cache lands in `tools/html5lib/.cache/`, which is gitignored: the suite is
third-party test data at a fixed revision, and a committed copy would rot out of
sync with the pin.

Two revisions are fetched, because the corpus has two homes:

| source | revision | files | what it is |
| --- | --- | --- | --- |
| `html5lib/html5lib-tests` | `9329e64694e7835d0dcff9811e22856ef6ad16f9` | 57 `tree-construction/*.dat` | the suite everyone means by "html5lib tests" |
| `web-platform-tests/wpt` | `c7fdee80f3f17b4e9813964916afdfd57ace863f` | 61 `html/syntax/parsing/resources/*.dat` | where those files live now |

**Pin `9329e64`, not `HEAD`.** html5lib-tests' HEAD commit
(`224991ec10db04f056a89eed8b0bd8695fd2950e`, 2026-06-26) is titled *"Tree
construction tests have moved to WPT"* and **deletes** `tree-construction/`,
`tokenizer/`, `serializer/` and `encoding/`. Pinning HEAD would fetch an empty
corpus and report a vacuous 100%. `9329e64` is the parent: the last revision that
still contains the tree-construction tests.

The WPT revision is the same one `tools/fetch-wpt.ps1` pins, and its copy is a
strict superset: four files the html5lib copy no longer carries
(`processing-instructions.dat`, `scripted_adoption01.dat`, `scripted_ark.dat`,
`scripted_webkit01.dat`) plus any edits to the shared ones.

The fetch is a `codeload.github.com` tarball per source, extracted by path prefix.
It needs no credentials. The WPT tarball is about 100 MB and takes minutes; the
corpus out of it is 61 small files. The script refuses to leave a partial suite
behind rather than reporting a number from half a corpus.

## Running it

```sh
cargo run -p render-html --example html5lib                    # the report
cargo run -p render-html --example html5lib -- --suite html5lib   # one source only
cargo run -p render-html --example html5lib -- --explain        # both trees per failure
cargo run -p render-html --example html5lib -- --filter "adoption01.dat#5"
cargo run -p render-html --example html5lib -- --negative-control
```

`RENDER_HTML5LIB_CACHE` overrides the cache directory.

`tools/html5lib/triage.py` groups the runner's compact failure list by mechanism
and prints the distinct inputs for each, which is how the ranked mechanism table
in the report was read off.

## The negative control

```sh
cargo run -p render-html --example html5lib -- --negative-control
```

A runner that reports "0 failures" because it silently matched nothing is the
failure mode this guards against, and a pass count cannot detect it: "0 failures"
and "0 comparisons" print the same. So the suite's own expectations are mutated
four ways — the `body` element renamed, the last tree line dropped, the root's
children swapped, and every text node and attribute value perturbed — and the
runner is re-run over exactly the cases each mutation changed.

What is counted is **cases that were passing and stop passing**. A mutation that
merely re-reports cases which were already failing has proved nothing however
many there are, so those are not counted, and the report says both numbers. The
control exits non-zero if any mutation is undetected *or* vacuous.

## What is and is not compared

**Compared:** the whole tree — node kinds, element local names, namespaces,
attribute names and values, children in order, template contents, and doctypes.

**Not compared:** the `#errors` sections. The suite says only how *many* parse
errors a conformant implementation reports, and different implementations have
different vocabularies for them. That is a separate conformance requirement
(13.2.2) and it is reported as a separate line, labelled as not part of the tree
figure, because a parser can build the right tree and still report the wrong
number of errors.

**Not compared:** the `#document-fragment` serialisation, because the fragment
parsing algorithm those tests exercise is not implemented at all.

## The `.dat` format

Hand-written reader, `std` only, no dependency. Two things in it are not obvious
and both were bugs before they were features:

- **Newlines are not escaped.** A text node holding a newline is written as an
  opening quote, the value with its newline in it, and a closing quote, so a node
  can span several physical lines. Reading the tree one line at a time truncates
  the value and reports a "missing node" that is not missing.
- **A node's terminator is its closing sequence followed by the end of a line**,
  and the opening delimiter is consumed first. Without that, the empty value
  `""` reads as an unterminated node and a value containing its own closing
  character ends early.
