# render-net

`render-net` is rENDER's bounded HTTP/HTTPS **transport adapter**. It accepts
already-normalized `url::Url` values and provides typed responses, explicit
resource limits, cancellation, and background/concurrent loading.

It deliberately does not implement the browser Fetch standard, CORS, CSP,
cookies, HTTP cache semantics, service workers, content sniffing, document
decoding, or navigation/history policy. Those semantics belong above this
crate. TLS uses rustls with its normal Web PKI verification and this crate does
not expose a switch to disable certificate verification.

The synchronous `HttpTransport` API is useful on engine/network threads.
`NetworkWorker` provides typed channels so a GUI/event-loop thread never needs
to perform blocking network I/O. Batch results always retain input order even
when requests complete out of order.

## Every request has a visible terminal outcome

A request either returns a `FetchResponse` or fails with a `FetchError`. A
failure that happened inside a network operation is tagged with the
`FetchPhase` it failed in and the time it had been running, so a stall is never
silent:

```text
render-net FAIL GET https://cdn.example/style.css: tcp connect: request timed out after 5007ms
render-net SLOW GET https://cdn.example/app.js 200 91823 bytes in 2036.4ms (slow threshold 1000ms)
```

- `FetchError::phase()` and `FetchError::elapsed()` read those two fields
  programmatically; `FetchError::into_inner()` returns the original typed
  variant for callers that branch on it. Failures raised before any I/O
  (request validation, URL handling, cancellation, redirect policy) stay
  untagged - there is no phase to blame.
- `FetchConfig::observer` receives every outcome as a `FetchEvent`. The default
  `StderrObserver` prints failures and slow requests, and every request when
  `RENDER_NET_LOG=1`; the slow threshold is 1s and `RENDER_NET_SLOW_MS=<ms>`
  changes it. `NullObserver` discards them.

## Bounds

- `FetchConfig::timeout` is a per-phase budget (resolve, send, header
  reception), not a whole-transfer cap; the body phase is bounded by
  `max_body_bytes`, a per-read idle bound, and a minimum-progress floor, so a
  large resource that keeps making progress is never killed.
- `FetchConfig::connect_timeout` (5s by default) bounds the connect phase only.
  ureq 3.3 walks the resolved addresses in order and gives each one a slice of
  that budget taken from a geometric series, so the first address receives about
  two thirds of it for a dual-stack host. A dedicated budget is what keeps one
  unresponsive AAAA record from costing ~16s of a 30s request; a zero value
  disables the bound.
- `BatchOptions::timeout` (30s by default, matching the shell's stall report)
  bounds a whole batch. Requests that had not completed are reported as
  `FetchPhase::Queued` failures carrying the time the batch ran for, instead of
  leaving the batch handle open forever. A caller whose resources legitimately
  need longer raises the budget; zero disables it.
- `FetchConfig::idle_connections_per_origin` and `idle_connections_total` bound
  what the pool keeps; requests in flight are not counted against either. See
  "The same-origin ceiling" above.

## Proxies

`HttpTransport::new` resolves a proxy from the environment
(`ALL_PROXY`/`HTTPS_PROXY`/`HTTP_PROXY` with `NO_PROXY`) or, on Windows, the
system proxy settings, and `HttpTransport::with_proxy` is the injection point
for a caller's own policy. Two rules apply:

- A proxy resolved implicitly never captures loopback targets. Browsers exclude
  `localhost` and the loopback ranges from an implicit proxy for the same
  reason: a proxy that cannot serve this machine costs an extra hop per
  request, and a local development server is not a remote resource. A proxy the
  caller hands in is explicit policy and applies to every target.
- Local addresses listed in `NO_PROXY` bypass the proxy as usual.

## Connection reuse

The pool works, and it is the reason the transport pins `Accept-Encoding` to
`gzip`. Measured with `examples/reuse_probe.rs` (three requests to one origin,
counting accepted connections):

| Advertised encoding | Connections used |
| --- | --- |
| `gzip` (what this transport sends) | **1** |
| identity | **1** |
| `gzip, br` | **3** |

ureq 3.3's brotli reader reaches the end of the *decoded* stream without
draining the length-delimited wire body, so the connection is never returned to
the pool and every request pays a fresh TCP connect and TLS handshake. That is
what made an earlier measurement read "135ms then 134ms, no reuse" on a
transport whose pool was working the whole time. `tests/connection_reuse.rs`
pins both halves, including a canary that fails if a future ureq makes brotli
poolable - at which point `br` can be advertised again.

### The same-origin ceiling

`FetchConfig::idle_connections_per_origin` (default
[`DEFAULT_PER_ORIGIN_CONCURRENCY`], 6) and `idle_connections_total` (default
`6 * ORIGINS_BEFORE_TOTAL_CEILING`, 24) replace ureq's defaults of 10 total and
**3 per host**. The per-host default of 3 was binding: a page pulling 40 assets
from one origin over HTTP/1.1 can have 6 requests in flight against it, so every
wave after the first reopens the connections the pool had just discarded.

Measured with `reuse_probe burst-compare` - a local origin serving 40 distinct
keep-alive paths, fetched in waves of 6, five runs each:

| Idle per origin / total | Connections for 40 assets |
| --- | --- |
| 3 / 10 (ureq defaults) | 10, 8, 11, 11, 13 |
| 6 / 24 (this crate) | 6, 6, 6, 6, 5 |

Wall clock on the same runs ranged from 15ms to 315ms *for the same
configuration* - this machine is shared with five other agents, so timing here
measures scheduling noise, not the transport. The connection count is
deterministic and is the mechanism: it halves, to the floor for the shape (six
requests in flight need six sockets). `reuse_probe burst` sweeps the per-origin
value from 1 to 12 and shows the curve is flat from 5 upwards, so the choice
sits on the knee rather than past it.

Six is not a number picked for its size: it is the per-origin concurrency
`BatchOptions::default()` already applied, and keeping fewer idle connections
than may be in flight throws away sockets the next request would have taken
while keeping more holds sockets that could never be reused. Both defaults read
`DEFAULT_PER_ORIGIN_CONCURRENCY`, and a test asserts they still agree through
the policy and not only through the constant.

The total ceiling is not measurably binding at one to three origins - with the
batch's 8-wide window, three origins never hold 6 connections each at once, and
`reuse_probe burst-origins` measures the same connection count with a total of
10 and of 24. It is set explicitly anyway, at four origins' worth, because a
total of one origin's width would truncate a page that loads from a CDN.

### `max_idle_age` is inert, and deliberately not exposed

ureq 3.3's `Connection::age()` is `now.duration_since(now)`, so it always
returns zero: `max_idle_age` can never evict anything and `Pool::get` can never
skip an aged connection. This crate therefore does **not** expose it - offering a
setting that provably does nothing is worse than not having it, and a real
policy would mean replacing the agent's pool, which is not reachable from here.

It is not needed for correctness. A pooled connection is probed by `is_open()`
before it is handed to a request, so an origin that closed an idle socket costs
one wasted pool slot and one new handshake rather than a failed request, which
`a_pooled_connection_the_origin_closed_is_discarded_not_failed` demonstrates.

## HTTP/2

Not supported, and not implementable inside this crate's dependency rules.
ureq 3.3 is an HTTP/1.1-only client: its feature list has no HTTP/2 flag, and
neither it nor `ureq-proto` contains a frame codec, HPACK, or a multiplexer -
the only mentions of HTTP/2 in either are comments saying it is unsupported.

The deeper problem is the shape of ureq's transport, not the missing codec. A
pooled `Connection` is *removed* from the pool while a request uses it and
returned by `Connection::reuse` afterwards, and `run()` holds `&mut Connection`
for the whole request: exactly one in-flight request per connection. HTTP/2 is
the inverse - one connection owning many concurrent streams - so multiplexing
cannot be expressed inside ureq's `Connector`/`Transport`/pool model at all.
It would mean writing an h2 client against the `unversioned` socket API (frames,
HPACK, flow control, stream scheduling, its own connection registry) or adding
`h2` or `hyper` and replacing the client outright, and either route puts the
system proxy path, the rustls configuration, the pooling behaviour above and the
connect budget at risk. A working HTTP/1.1 path is worth more than that, so the
connection-count problem is attacked structurally instead: one pooled
connection per origin, and a bounded connect phase so a dead address is cheap.

ALPN: because ureq 3.3 has no ALPN API, this transport sends no ALPN extension,
and RFC 7301 lets a server pick any protocol in that case. Measured on the
hosts in the real-site evidence (`g.alicdn.com`, `mat1.gtimg.com`,
`static.ws.126.net`, `www.taobao.com`), all four serve HTTP/1.1 to a no-ALPN
client and an HTTP/1.1 request line works, so this has not bitten. Pinning ALPN
to `http/1.1` would make it deterministic, but it needs ureq to expose the
rustls `ClientConfig`, which it does not.

## Known limits

- ureq 3.3 has no happy-eyeballs: addresses are tried one after another, not
  raced. `connect_timeout` bounds the cost of a dead address but cannot make
  failover faster, and there is no per-host record of which address family
  works, so every request to a dual-stack host with broken IPv6 pays one dead
  connect first. Racing needs a custom `Connector`/`Transport` pair built on
  ureq's `unversioned` API, which this crate does not do.
- The phase in a failure is exact for timeouts (ureq reports which of its
  budgets expired) and best-effort otherwise: a non-timeout socket failure is
  reported against the connect phase, which is where the overwhelming majority
  of them come from.
- `elapsed` on a hop failure covers everything ureq does in one call - resolve,
  connect, TLS, send, and response headers - because only ureq can see the
  boundaries between those. A request served on a pooled connection has no
  connect phase of its own; it is still named, bounded, and reported per
  request (`a_reused_connection_still_gets_a_named_bounded_outcome_per_request`).
- Under HTTP/2 the same API would still report one outcome per request, but the
  phases describe a stream rather than a connection, and the connect phase
  would be paid once per connection instead of once per request. Nothing here
  depends on that today.

## Diagnostics

`cargo run -p render-net --example connect_probe -- <command>`:

- `addr <host:port>` resolves a host and times a bare `TcpStream` connect to
  every resolved address, in order: the ground truth for what the first
  address costs, independent of the transport.
- `fetch <url>...` fetches through `HttpTransport` and prints the terminal
  outcome with its elapsed time. `--timeout <ms>` / `--connect <ms>` set the two
  budgets, `--no-proxy` bypasses an implicit proxy, `--repeat <n>` repeats each
  URL to expose connection reuse, `-v` turns on ureq's own logs.

`cargo run -p render-net --example fetch_bench -- <url>...` is the older
per-URL latency probe and takes the same budget flags.

`cargo run -p render-net --example reuse_probe` stands up a local origin that
counts accepted connections and reports how many three requests cost, per
content coding. `REUSE_PROBE_TRACE=1` additionally prints ureq's own
`Return to pool` / `Use pooled` lines, which is how the brotli mechanism was
identified. Sub-commands measure the page shape:

- `burst` sweeps the per-origin idle ceiling from 1 to 12 against a 40-asset
  single-origin page.
- `burst-compare` runs that page with ureq's defaults and with this crate's.
- `burst-origins` runs it across three origins, which is how the total ceiling
  was found not to bind.
