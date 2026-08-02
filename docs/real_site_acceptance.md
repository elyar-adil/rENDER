# Offline Real-Site Acceptance

The repository keeps five deterministic page fixtures under
`tests/fixtures/real_sites`:

- Baidu home page
- Baidu search results
- Zhihu home page
- Zhihu article page
- NetEase (`www.163.com`) portal home page

They are reduced snapshots of common page shapes, not alternate runtime
implementations. The fixtures retain real-site URL shapes and external CSS,
image, and script references, while tests replace resource responses with
deterministic local values. No acceptance test requires Internet access.

## Coverage

`tests/test_real_site_capabilities.py` checks each fixture for:

- a decoded document title;
- header, navigation, main content, article, section, and aside semantics;
- usable links;
- a `role=search` form with a named input and submit control;
- stylesheet, image, and script resource classification;
- block layout that continues below a 600px viewport;
- ordered text blocks in the scroll region.

The 163 fixture additionally preserves the resource shapes observed on the
live portal: `static.ws.126.net` and `nimg.ws.126.net` assets, `img.lazy`
elements using `data-src`/`data-original`, and an `srcset` candidate list.
Its offline checks require usable lazy-image source candidates, alt text, a
long channel feed, a search form, and navigable channel links. The Rust
browser-level check runs the image discovery plan and permits the expected
`MissingSource`/`SrcsetUnsupported` warnings for deferred images, but fails on
any image discovery `Error`.

The standalone Rust harness in `tests/real_site_tasks` repeats the document,
resource, form, link, and long-content checks against the same fixtures. Site
names are test labels only. The browser core must satisfy these contracts
through its normal HTML, CSS, layout, paint, and resource paths.

## Commands

```powershell
pytest -q tests/test_real_site_capabilities.py
cargo test --manifest-path tests/real_site_tasks/Cargo.toml --test real_site_capabilities
cargo test --manifest-path tests/real_site_tasks/Cargo.toml --test live_http_smoke -- --ignored --nocapture
```

These checks are an offline compatibility gate. They do not claim that login,
personalized feeds, anti-bot flows, or every JavaScript interaction on the live
sites is implemented. The second command is an explicit opt-in HTTP smoke
using only `render-net`; it checks the Baidu, Zhihu, and NetEase endpoints for a 2xx response,
non-empty content, and HTML. DNS, timeout, TLS, and other network-availability
errors are reported as skips so an unavailable network does not become an
ordinary test failure. A reachable endpoint with a non-2xx response, empty
body, or non-HTML response still fails.
