//! Scratch latency probe: prints per-request wall time for URLs given on the
//! command line. Repeat a URL to expose connection pooling behavior.

use std::io::Read;
use std::time::Instant;

use render_net::{CancelToken, FetchConfig, FetchRequest, HttpTransport};

struct ScratchLogger;

impl log::Log for ScratchLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Debug
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[ureq] {}", record.args());
        }
    }
    fn flush(&self) {}
}

fn raw_ureq_probe(url: &str) {
    let agent_config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .build();
    let agent: ureq::Agent = agent_config.into();
    for round in 1..=2 {
        let start = Instant::now();
        let response = agent.get(url).call().expect("ureq call");
        let mut bytes = 0usize;
        let mut chunk = [0_u8; 16 * 1024];
        let mut reader = response.into_body().into_reader();
        loop {
            match reader.read(&mut chunk).expect("body read") {
                0 => break,
                count => bytes += count,
            }
        }
        drop(reader);
        println!("raw-ureq #{round}  {:>9.1?} {bytes} bytes", start.elapsed());
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut verbose = false;
    let mut urls = Vec::new();
    for arg in args.by_ref() {
        if arg == "-v" {
            verbose = true;
        } else if arg == "raw" {
            // Legacy mode marker: treat the rest as raw-ureq probe URLs.
            for url in args.by_ref() {
                raw_ureq_probe(&url);
            }
            return;
        } else {
            urls.push(arg);
        }
    }
    if verbose {
        let _ = log::set_boxed_logger(Box::new(ScratchLogger));
        log::set_max_level(log::LevelFilter::Debug);
    }
    let transport = HttpTransport::new(FetchConfig::default());
    let cancel = CancelToken::default();
    for url in &urls {
        let Ok(parsed) = url.parse() else {
            println!("INVALID   {url}");
            continue;
        };
        let request = FetchRequest::new(render_net::HttpMethod::Get, parsed);
        let start = Instant::now();
        let result = transport.fetch(&request, &cancel);
        let elapsed = start.elapsed();
        match result {
            Ok(response) => println!("{elapsed:>9.1?} {} bytes  {url}", response.body.len()),
            Err(error) => println!("{elapsed:>9.1?} ERROR {error}  {url}"),
        }
    }
}
