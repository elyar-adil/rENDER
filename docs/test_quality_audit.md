# Test Quality Audit

A register of **surviving mutations**: places where the smallest change that
should break a behaviour left the whole suite green.

`docs/visual_fidelity_gaps.md` records capability gaps, and its own conclusion is
that a passing test count is not evidence of quality. This file turns that into a
measurement, in the same style: a table of findings, each with a `file:line`, the
mutation, the outcome, and for a survivor the thing that is *not* being tested.

Method: for each behaviour, make the smallest possible change that should break
it - invert a condition, change a constant, delete a branch, swap a comparison -
and run only the tests that claim to cover it. No `cargo-mutants`: it would be a
dependency on a project rule of "small audited crates only", and it would fight
the rest of the session for the build lock. Every mutation was reverted by
re-`edit`ing the line, and the revert was verified by SHA-256 against a copy taken
before the mutation (the tree is shared, so "reverted" had to mean byte-identical,
not "looks right").

Line numbers are the state as of 2026-09-28, with the in-flight work of other
agents present.

---

## The denominator, stated first

A survival rate without a sample size reads as coverage, so here it is up front.

| | |
| --- | --- |
| Mutations attempted | **34** |
| Killed (a test went red) | **24** |
| Survived (whole suite green) | **10** |
| Survival rate | **29%** (10/34) |
| Crates reached | `render-net`, `render-css`, `render-dom`, `render-layout` |
| Assertions-free test functions found | **10** (of which 5 are real) |
| Files mutated and reverted, byte-identical | **9** |

Per area:

| Area | Mutations | Killed | Survived |
| --- | --- | --- | --- |
| `render-layout` table solver | 8 | 5 | 3 |
| `render-layout` sticky | 5 | 4 | 1 |
| `render-layout` scrollport | 3 | 3 | 0 |
| `render-css` `@supports` | 6 | 4 | 2 |
| `render-css` `text-decoration` | 2 | 1 | 1 |
| `render-net` phase budgets | 7 | 5 | 2 |
| `render-dom` form owner | 3 | 2 | 1 |
| **Total** | **34** | **24** | **10** |

**This is not coverage.** It is a sample of 24 hand-picked behaviours, weighted
towards the newest and most load-bearing code by hand. A surviving mutation here
means "nothing I tried in one sitting noticed", not "nothing exists".

### What was not reached, and why

- **`tests/real_site_tasks/`** - read, never written, never run, per the
  instruction that another agent is mid-edit there. Two findings below come from
  reading only and are marked as such.
- **The 12 timing tests' wall-clock robustness.** I verified each of the 12 is
  *reached* by a mutation; I did not verify each is *stable* on a loaded machine,
  which needs a slow-CI run and would have taken the build lock for minutes.
- **`render-core`, `render-browser`, `render-html`, `render-js`** - no mutations.
  `render-browser` does not build (in-flight font-axis work, already reported to
  the orchestrator and not treated as a finding here). `render-js` and
  `render-html` were moving under me; I spent the time on the four crates the
  brief ranked highest instead, and say so rather than pretending the sample
  covers the tree.
- **`render-layout`'s inline/flex/grid solvers, `render-css`'s cascade
  specificity, `render-dom`'s tree mutation** - not reached.
- **One deliberate non-mutation**: a first attempt at the `caption_side` mutation
  was semantically a no-op (it moved the `!` as well as the keyword, which
  cancels out). It is *not* counted in the 34. The corrected inversion was
  counted, and killed. Counting a no-op as a survivor would have been a fake
  finding, which is the failure mode this file exists to catch.

---

## Findings, ranked by the cost of the defect they would have caught

| # | `file:line` | Mutation | Caught by | Not tested / what a real defect would look like |
| --- | --- | --- | --- | --- |
| 1 | `crates/render-layout/src/solver/table.rs:821` | `spacing * count_as_f32(columns.saturating_add(1))` -> `* count_as_f32(columns)` in `fixed_table_column_widths` - one border-spacing gap dropped | **nothing** (47 table tests green) | **`table-layout: fixed` with a non-zero `border-spacing` is untested.** Every auto-layout table is pinned by `border_spacing_separates_every_column_and_the_table_edges` (table_tests.rs:797, which asserts `200 - 3*7 = 179`); the fixed-layout test (table_tests.rs:736) uses `border-spacing: 0`. A real defect: every `position: fixed`-layout table on a site that sets `border-spacing: 2px` computes its columns 2px too wide each, and the table's right edge overflows by 2px. Table layout is the most-used feature in the corpus. Control: the *same* off-by-one in the auto path (table.rs:731) **is** killed by that test, so this is a hole in one branch, not a weak assertion. |
| 2 | `crates/render-layout/src/solver/table.rs:173` | `(self.width, style_precedence(self.style)) > (other.width, style_precedence(other.style))` -> `(style_precedence(..), width) > (style_precedence(..), width)`: style precedence now outranks width | **nothing** | **CSS 2.1 §17.6.2's "a wider border beats a narrower one" is untested.** The two *trumps* are covered - `collapsed_border_winner` first-claim-always-wins was killed by `the_table_border_wins_the_outer_grid_lines` and `a_hidden_border_wins_the_conflict_and_takes_no_space` - but the width-vs-style ordering, the third and most common rule, has no test. A real defect: on a `border-collapse: collapse` table where one cell has `1px dashed` and its neighbour `2px dotted` on the shared line, the *dashed* 1px border wins and paints; the grid line is the wrong width and the wrong style for the whole run. `docs/visual_fidelity_gaps.md` claims §17.6 "with conflict resolution" - only the trumps are resolved. |
| 3 | `crates/render-layout/src/solver/table.rs:179` | `style_precedence`: `Double => 9, Solid => 8` -> `Double => 8, Solid => 9` | **nothing** | **The style tie-break list itself is untested.** §17.6.2's explicit ordering (`double`, `solid`, `dashed`, `dotted`, `ridge`, `outset`, `groove`, `inset`) is a data table in the source and nothing reads it. A real defect: equal-width `solid` and `double` on a shared line resolve the wrong way, so a `double` rule renders as a single hairline (or vice versa) along every grid line it touches. A mutant that reorders two arbitrary entries would also survive. |
| 4 | `crates/render-layout/src/sticky.rs:162` | `(None, Some(_)) => (view_end - box_end).min(0.0)` -> `.max(0.0)` | **nothing** (all 199 `render-layout` lib tests green) | **`position: sticky; right:` and `bottom:` have no test at all.** All 12 tests in `sticky_tests.rs` use `top` or `left`; the mirror arm is dead to the suite. A real defect: a sticky footer (`bottom: 0`) or a right-rail sticky element (`right: 0`) is never constrained on that edge, so it never sticks - the single most common shape after a sticky header, and one the corpus exercises. The doc comment at sticky.rs:45-53 ("`bottom: 0` never pushes one down") describes behaviour nothing checks. |
| 5 | `crates/render-css/src/cascade.rs:1102` | `line.unwrap_or_else(\|\| "none".to_owned())` -> `"underline".to_owned()` | **nothing** (all 183 `render-css` lib tests green) | **The *omitted* `text-decoration-line` slot is untested, while the other three initials are pinned.** The brief asked exactly this and the answer is split: `style` -> `wavy` was killed by 4 tests, `thickness`/`color` are asserted directly, but every one of the 10 `text-decoration` tests names a line keyword explicitly, so the line slot's initial value is never exercised. A real defect: `text-decoration: 2px wavy blue` (legal under Text Decoration 4 §2.6's `||` with the line omitted) silently *adds* an underline where the author wrote none - the exact mirror of the bug this shorthand was written to fix (cascade.rs:996-1003), and the same user-visible symptom, underlines on a nav. |
| 6 | `crates/render-net/src/transport.rs:1285` | `if elapsed > idle_timeout` -> `if false && ...`: the minimum-progress floor deleted | **nothing** in `phase_budgets` | **`MIN_BODY_BYTES_PER_SECOND` (transport.rs:25) is enforced by no test anywhere.** It is the only bound on a connection that trickles bytes below the idle bound, and it is the thing that stops a slow-drip resource pinning a worker thread forever. A real defect: a server delivering 100 bytes every 500ms against a 600ms idle bound holds a thread for the whole download with no floor to stop it. The one test that mentions the floor (`a_slow_large_body_outlives_every_total_budget_and_still_completes`, phase_budgets.rs:707-708) says in a comment that it deliberately stays *above* the floor - so the floor has an explicit excuse and no test. |
| 7 | `crates/render-css/src/supports.rs:535` | `"env" => Outcome::answered(false)` -> `Outcome::answered(true)` | **nothing** | **`@supports (… env(…))` is answered and nothing checks the answer.** Level 5 §2.1.5 says a feature is supported if the ident names a variable the engine substitutes; there is no `env()` substitution in the computed-value stage, so `false` is the true statement. A real defect, and the *same shape* as the at-rule lie this module exists to prevent: a page writing `@supports not (color: env(--brand))` loses its fallback, and the engine claims a capability it does not have. The source comment at supports.rs:530-534 says "Adding `env()` support has to change this line" - nothing enforces it. |
| 8 | `crates/render-css/src/supports.rs:529` | `"named-feature" => Outcome::answered(IMPLEMENTED_NAMED_FEATURES.contains(&argument))` -> `Outcome::answered(true)` | **nothing** | **`named-feature(...)` answers `true` for every name and the closed list `IMPLEMENTED_NAMING` is never read by a test.** §2.1.3 fixes the list as closed, and `IMPLEMENTED_NAMED_FEATURES` is empty (supports.rs:252). A real defect: `named-feature(anchor-position-follows-transforms)` reports supported, so a page's `not named-feature(...)` fallback is dropped. Same class as #7, same missing enforcement comment. |
| 9 | `crates/render-dom/src/lib.rs:655` | `form_owner`'s parser-inserted link: drop the `is_in_same_tree(node, link.form)` + `NodeKind::Element` revalidation, trust the recorded pointer | **nothing** (all 29 `render-dom` tests green) | **The recorded form owner is never re-validated against the tree, and no test covers the form being removed.** `a_parser_inserted_owner_is_ignored_once_the_element_moves` moves the *control*; nothing moves or removes the *form*. A real defect: the HTML parser's form-element-pointer path records a non-ancestor owner; if the form is then removed (script, or reparenting), the control keeps an owner pointing at a detached form, so it submits into a form that is not in the document and is missing from every real form's element list. The brief's note - these tests "encode a design decision (derive on read, store exactly one datum) and would fail confusingly if that decision were quietly undone" - is confirmed: a *partial* undo of the revalidation is invisible. |
| 10 | `crates/render-net/src/transport.rs:1268` | `Err(RecvTimeoutError::Timeout) => break Err(FetchError::Timeout)` -> `continue`: the channel-side idle bound neutered | **nothing** in `phase_budgets` | **The channel half of the body idle bound is redundant with the socket half and nothing pins it.** `phase_budgets.rs:14-18` says the budget is "set both on the channel and on the socket" and `transport.rs:1219-1225` says "the two must agree"; with the channel one neutered, ureq's `timeout_recv_body` fires first and produces the same error, phase and text, so the test passes. The stated purpose of the channel bound - serving a decoder that stalls without touching the socket (a gzip member that never completes) - is therefore unpinned. A real defect: delete the socket deadline as well (as the same author did once, and found) and there is no second line of defence; or keep only the socket one and a stalled *decoder* hangs forever. |

### Non-findings worth keeping (mutations that died, recorded so the work is not repeated)

These were killed, and the killing test is named. Where the killing set is small,
that is itself a fragility note.

| `file:line` | Mutation | Killed by | Note |
| --- | --- | --- | --- |
| `crates/render-net/src/transport.rs:885` | `timeout_recv_body(Some(..))` -> `None` | exactly 1: `a_body_timeout_releases_the_socket_instead_of_leaving_it_held` | This is the one the author claims to have verified by reverting. Confirmed: it is the only thing that holds it, and nothing else in the crate does. Fragile, and correct. |
| `crates/render-net/src/transport.rs:874` | `timeout_recv_response(None)` -> `Some(response_timeout)` | exactly 1: `a_slow_large_body_outlives_every_total_budget_and_still_completes` | Confirms the module's central asymmetry claim (RecvResponse is a *total* budget over headers+body) is load-bearing. |
| `crates/render-net/src/transport.rs:1046` | delete the `RequestSend` -> `ResponseHeaders` phase rewrite | 5 tests | Well covered. |
| `crates/render-net/src/transport.rs:558` | drop `.min(self.timeout)` on the connect clamp | 1: `the_three_phase_budgets_have_named_defaults...` | |
| `crates/render-net/src/transport.rs:573` | `effective_body_idle_timeout` ignores the configured value | 2 | |
| `crates/render-css/src/stylesheet.rs:1432` | `Evaluated(false)` arm applies the block (the 92-rule defect, re-created) | 3: `a_supports_condition_gates_its_block`, `supports_nests_with_media_in_both_orders`, `a_nested_supports_condition_gates_the_enclosing_rules_declarations` | **The assertion whose absence let 92 rules through now exists and is load-bearing.** Answering the brief's question directly: yes, something asserts that a false condition drops its block, and three tests go red if it stops doing so. |
| `crates/render-css/src/supports.rs:166` | `all(...)` -> `any(...)` over the shorthand's longhands | 1: `shorthand_support_is_the_conjunction_over_its_longhands` | |
| `crates/render-css/src/supports.rs:179` | `Some(Err(_)) => true` - a rejected value claims support | 3 | |
| `crates/render-css/src/supports.rs:561` | `AtRuleSupport::Unimplemented` -> `answered(true)` - the at-rule lie | 4 | |
| `crates/render-css/src/cascade.rs:1109` | `style` initial `solid` -> `wavy` | 4 | |
| `crates/render-layout/src/sticky.rs:161` | `(Some(_), None)` -> `.min(0.0)` (a `top` inset pulling a box up) | 9 | The best-covered branch in the sticky solver. |
| `crates/render-layout/src/sticky.rs:148` | delete the containing-block-visibility gate | exactly 1: `a_sticky_box_that_has_been_scrolled_past_is_not_pinned_to_the_scrollport` | Thin: one assertion holds a whole §4.1 rule. |
| `crates/render-layout/src/sticky.rs:167` | delete the `lowest > highest` branch | exactly 1: `a_sticky_box_larger_than_its_containing_block_is_left_alone` | |
| `crates/render-layout/src/sticky.rs:165` | `lowest` -> `-inf` | exactly 1: same test | Both containing-block travel bounds are held by a single test with a single assertion. |
| `crates/render-layout/src/solver/table.rs:731` | drop one spacing gap in the **auto** path | 1: `border_spacing_separates_every_column_and_the_table_edges` | The control for finding #1. |
| `crates/render-layout/src/solver/table.rs:137` | first competing border always wins | 2 | |
| `crates/render-layout/src/solver/table.rs:259` | `VerticalAlign::Middle => share` (behaves as bottom) | 1 | |
| `crates/render-layout/src/solver/table.rs:1269` | `empty-cells: hide` -> `show` | 1 | |
| `crates/render-layout/src/solver/table.rs:1352` | `caption_side` top/bottom inverted | 1 | |
| `crates/render-layout/src/solver/block.rs:58` | `any(...)` -> `all(...)` over the overflow axes | 1: `overflow_on_one_axis_only_scrolls_that_axis` | |
| `crates/render-layout/src/scrollport.rs:51` | a clip gets a scroll range | 2 | |
| `crates/render-layout/src/solver/mod.rs:788` | a clipping box stops being its own nearest scrollport | 1: `a_nested_scrollport_does_not_enlarge_the_one_around_it` | |
| `crates/render-dom/src/lib.rs:599` | drop the `img` exclusion from "listed" | 4 | |
| `crates/render-dom/src/lib.rs:669` | drop `is_form_element` from the `form=` lookup | 1: `a_form_content_attribute_that_resolves_to_nothing_leaves_no_owner` | |

---

## Every assertion-free test function in the tree

Proxy: a `#[test]` function whose body contains no `assert`/`assert_eq`/
`assert_ne`/`debug_assert`/`.is_err()`/`.is_ok()`/`unwrap_err`/`expect_err`/
`should_panic`, and no call to a local helper whose own body has one. Scanned
over every `.rs` file under `crates/`, `tests/` and `tools/` (1313 `#[test]`
attributes), skipping comments, raw strings and byte strings.

| `file:line` | Verdict |
| --- | --- |
| `crates/render-js/src/runtime/tests.rs:2199` `temp_diag_mutual_recursion` | **Real.** A `#[test]` (not `#[ignore]`d) that `eprintln!`s and returns. Reads `.diag/bilibili/page.html` and `.diag/bilibili/assets/a001_log-reporter.js`; **both files are present**, so it runs, passes, and is counted. Names a live site in its URL (`https://www.bilibili.com/`, tests.rs:2211). This is `scratch_noscript_probe` again, in a test count, after it was removed from `tree_builder.rs`. |
| `crates/render-js/src/runtime/tests.rs:2635` `temp_read_5073_state` | **Real.** Same shape; the fixture files are present, so it runs and passes. |
| `crates/render-layout/src/solver/tests.rs:112` `probe_live_163_navigation_width` | **Real but quarantined.** `#[ignore = "manual fixture probe..."]`, so it does *not* inflate a count - the right call, and worth naming as the counter-example. It is also site-specific in source: a hardcoded `NodeId::from_u64(1047)` (tests.rs:201) against a saved 163 homepage. |
| `crates/render-js/src/video/present.rs:784` `frame_publication_carries_node_and_url` | **Real.** Constructs a `FramePublication` and drops it. The name claims the struct "carries node and url"; nothing reads either field. Cannot fail. |
| `tests/real_site_tasks/tests/probe.rs:18` `probe_form_owners` (untracked, in flux) | **Real.** The file header says "TEMPORARY measurement probe. Deleted before the round closes." It `println!`s form-owner geometry and asserts nothing. Another agent owns this directory; read only. |
| `crates/render-core/tests/test262.rs:1230` `official_default_harness_compiles_when_checkout_is_present` | Benign. Early-`return`s when `third_party/test262` is absent, which is a documented skip, not a vacuous assertion; when the checkout is present it `expect`s every harness source to compile. |
| `crates/render-net/src/diagnostics.rs:225` `null_observer_accepts_events_without_observer_state` | Benign. The assertion is "does not panic" on a no-op observer, which is a real assertion in Rust - the test fails if `on_fetch_event` panics. The heuristic over-reports this one. |
| `tests/real_site_tasks/tests/render_diff.rs:30` `renders_every_fixture_and_reports_the_baseline_difference` | **Real, and documented.** Its own header (lines 1-23) says "this test passes unconditionally", that no baseline is checked in, and that a pixel gate failing while known gaps are open "trains everyone to ignore it". A deliberate decision, not an oversight - but it means the file renders every fixture and asserts nothing, so a *crash* in the render path is the only failure it can report. |
| `crates/render-html/src/tree_builder.rs:5887`, `:5930` `parse_without_scripting` | False positives: local helper `fn`s nested inside two `#[test]`s. The enclosing tests assert. |
| `crates/render-js/src/runtime/tests.rs:2199` (nested), `crates/render-browser/src/font_matching.rs:1549`, `:1591` | False positives in the first pass; the render-browser ones are in another agent's untracked in-flight file and were excluded from the reported set. |

Also observed while reading, not a test-quality finding: `tests/real_site_tasks/tests/zz_probe.rs`
is a 445-byte file of spaces.

---

## Duplicate coverage masquerading as breadth

No duplicate `#[test]` function names exist anywhere in the tree, and no
`assert!(x == x)` / `assert_eq!(x, x)` / `assert!(true)` forms exist. Those two
cheap checks came back clean. The duplication that does exist is *semantic*, and
it is in the new timing suite:

| New test | Pre-existing test it re-asserts | Assessment |
| --- | --- | --- |
| `crates/render-net/tests/phase_budgets.rs:464` `a_server_that_never_answers_ends_as_a_named_terminal_outcome` | `crates/render-net/tests/connect_budget.rs:193` `a_server_that_never_answers_headers_names_the_header_phase_and_elapsed_time` | The older test asserts the same fixture, the same `Some(FetchPhase::ResponseHeaders)`, the same `FetchError::Timeout`, and the same "the message names the phase and the elapsed milliseconds". The new one adds the exact terminal *string* and the observer line, so it is a superset - but the pair reads as two independent guards of one behaviour. |
| `crates/render-net/tests/phase_budgets.rs:654` `a_body_that_stops_arriving_ends_as_a_named_terminal_outcome` | `crates/render-net/tests/local_transport.rs:851` `stalled_body_reads_time_out_with_a_typed_error` | The older test asserts exactly `phase == BodyTransfer` and `into_inner() == Timeout` and nothing else. Strict subset. |
| `crates/render-net/tests/phase_budgets.rs:699` `a_slow_large_body_outlives_every_total_budget_and_still_completes` | `crates/render-net/tests/local_transport.rs:825` `slow_but_progressing_bodies_are_not_spuriously_timed_out` | Both assert "a trickling body outlives a total budget and completes". The new one additionally outlives the *response* budget, which is the point of the suite - so this pair is a genuine addition, and the earlier finding (killed only by the new test) shows it. |
| `crates/render-css/src/supports.rs:950` `the_declaration_oracle_answers_from_the_engine_not_from_a_table` | `crates/render-css/src/supports.rs:655` `a_declaration_condition_is_answered_by_the_declaration_oracle` | Near-identical property/value pairs, in both directions, differing only in whether the `( … )` wrapper is present. The pair reads as 2 tests and is closer to 1 plus a wrapper check. |

Of the "12 new timing tests", two are strict or near-strict subsets of tests that
already existed in other files. The new load-bearing count is nearer 10 than 12.

---

## What to do about it, cheapest first

1. `table_tests.rs`: add a `table-layout: fixed` + non-zero `border-spacing` case
   (one test, four assertions) and a `border-collapse: collapse` case with two
   *equal-width, different-style* borders on a shared line. That closes #1, #2
   and #3 - the three findings on the most-used feature in the corpus.
2. `cascade.rs`: one test with `text-decoration: 2px wavy blue`, asserting the
   line slot is `none`. Closes #5.
3. `sticky_tests.rs`: one test with `position: sticky; bottom: 0` in a tall
   containing block, asserting the box stops at its bottom edge. Closes #4.
4. `supports.rs`: two `assert!`s on `named-feature(...)` and `env(...)`. Closes
   #7 and #8 for the cost of two lines.
5. Delete `temp_diag_mutual_recursion` and `temp_read_5073_state`. They are the
   `scratch_noscript_probe` pattern, they are live, and they are site-specific in
   source.
6. `tools/check.sh` already enforces site neutrality for *identifiers*; a
   `#[ignore]`-or-assert rule for new `#[test]` functions would stop this class
   recurring. The `#[ignore]` on `probe_live_163_navigation_width` shows the
   right answer already exists in the tree and just is not required.
