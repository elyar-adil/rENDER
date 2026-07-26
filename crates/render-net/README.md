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
