//! Offline real-site acceptance harness for the rENDER engine.
//!
//! The contract this crate exists to keep answerable is `docs/real_site_acceptance.md`:
//! *does a real-world page shape render correctly, offline and repeatably?* Until
//! this crate existed the only answer was to open a window and look.
//!
//! Three things live here:
//!
//! * [`fixture`] - the reduced real-site page shapes, read from
//!   `tests/fixtures/real_sites/`. They are snapshots of common page shapes
//!   with real-site URL shapes intact, not alternate runtime implementations.
//! * [`harness`] - one offline load per fixture through the engine's normal
//!   path (parse, discover author stylesheets, discover images, discover
//!   scripts, answer every request with a deterministic local value, render).
//! * [`diagnostics`] and [`diagnostic_set`] - the diagnostic stream a load
//!   produced, as a comparable set, and the per-fixture expected sets it is
//!   compared against. Diagnostics are a first-class output here because
//!   nothing else in the tree can currently display them; see the module docs
//!   for the rule the expected sets encode.
//! * [`inspect`] and [`shots`] - structural queries over the DOM and the
//!   fragment tree, plus report-only raster capture and baseline comparison.
//!
//! Site names appear only as test labels. Nothing below branches on a host, a
//! URL, or a document shape: the engine satisfies these fixtures through its
//! ordinary HTML, CSS, layout, paint, and resource paths, which is the law in
//! `docs/generic-browser-todo.md` and what `tools/check_site_neutrality.py`
//! enforces mechanically.
//!
//! No check in this crate requires Internet access. The single opt-in HTTP smoke
//! test is `tests/live_http_smoke.rs`, which is `#[ignore]`d and only runs when
//! asked for by name.

//! * [`contract`] - the acceptance contract, as pure checks over a [`harness::Session`].

pub mod contract;
pub mod diagnostic_set;
pub mod diagnostics;
pub mod fixture;
pub mod harness;
pub mod inspect;
pub mod shots;
