//! Scratch connect/latency probe.
//!
//! Two subcommands:
//!
//! * `addr <host:port>` - resolve `host` and time a bare `TcpStream` connect to
//!   every resolved address in order. This is the ground truth for "how long
//!   would the first resolved address cost us", independent of the transport.
//! * `fetch <url>...` - fetch through [`render_net::HttpTransport`] and print
//!   the terminal outcome (success, or the error with its phase and elapsed
//!   time) for each URL.
//!
//! Flags: `--no-proxy`, `--timeout <ms>`, `--connect <ms>`, `--attempt <ms>`
//! (bare connect budget for `addr`), `--repeat <n>`, `--verbose` (ureq logs).
//!
//! `--timeout` is the per-request budget and `--connect` the connect-phase
//! budget, so `--timeout 30000 --connect 700` shows what one unresponsive
//! address costs out of a 30s request.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

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

struct Options {
    no_proxy: bool,
    timeout: Option<Duration>,
    connect: Option<Duration>,
    attempt: Duration,
    repeat: usize,
    verbose: bool,
}

fn parse_options() -> (Options, Vec<String>) {
    let mut options = Options {
        no_proxy: false,
        timeout: None,
        connect: None,
        attempt: Duration::from_secs(5),
        repeat: 1,
        verbose: false,
    };
    let mut rest = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-proxy" => options.no_proxy = true,
            "--verbose" => options.verbose = true,
            "--timeout" => {
                let value = args.next().expect("--timeout needs a value");
                options.timeout = Some(Duration::from_millis(value.parse().expect("ms")));
            }
            "--connect" => {
                let value = args.next().expect("--connect needs a value");
                options.connect = Some(Duration::from_millis(value.parse().expect("ms")));
            }
            "--attempt" => {
                let value = args.next().expect("--attempt needs a value");
                options.attempt = Duration::from_millis(value.parse().expect("ms"));
            }
            "--repeat" => {
                let value = args.next().expect("--repeat needs a value");
                options.repeat = value.parse().expect("count");
            }
            _ => rest.push(arg),
        }
    }
    (options, rest)
}

fn probe_addresses(target: &str, attempt: Duration) {
    let start = Instant::now();
    let addrs: Vec<SocketAddr> = match target.to_socket_addrs() {
        Ok(addrs) => addrs.collect(),
        Err(error) => {
            println!("resolve {target} failed in {:?}: {error}", start.elapsed());
            return;
        }
    };
    println!(
        "resolve {target} -> {} address(es) in {:.1?}: {addrs:?}",
        addrs.len(),
        start.elapsed()
    );
    for (index, addr) in addrs.iter().enumerate() {
        let start = Instant::now();
        match TcpStream::connect_timeout(addr, attempt) {
            Ok(stream) => {
                let peer = stream
                    .peer_addr()
                    .map(|peer| peer.to_string())
                    .unwrap_or_default();
                println!(
                    "  [{index}] {addr} connected in {:.1?} ({peer})",
                    start.elapsed()
                );
            }
            Err(error) => println!(
                "  [{index}] {addr} failed in {:.1?}: {error}",
                start.elapsed()
            ),
        }
    }
}

fn main() {
    let (options, rest) = parse_options();
    if options.verbose {
        let _ = log::set_boxed_logger(Box::new(ScratchLogger));
        log::set_max_level(log::LevelFilter::Debug);
    }
    let Some((mode, args)) = rest.split_first() else {
        eprintln!("usage: connect_probe addr <host:port> | fetch <url>... [flags]");
        return;
    };
    match mode.as_str() {
        "addr" => {
            for target in args {
                probe_addresses(target, options.attempt);
            }
        }
        "fetch" => {
            let mut config = FetchConfig::default();
            if let Some(timeout) = options.timeout {
                config.timeout = timeout;
            }
            if let Some(connect) = options.connect {
                config.connect_timeout = connect;
            }
            let transport = if options.no_proxy {
                HttpTransport::with_proxy(config, None)
            } else {
                HttpTransport::new(config)
            };
            let cancel = CancelToken::default();
            for url in args {
                let Ok(parsed) = url.parse::<render_net::Url>() else {
                    println!("INVALID   {url}");
                    continue;
                };
                for round in 1..=options.repeat {
                    let start = Instant::now();
                    let result = transport.fetch(&FetchRequest::get(parsed.clone()), &cancel);
                    let elapsed = start.elapsed();
                    match result {
                        Ok(response) => println!(
                            "#{round} {elapsed:>9.1?} {} bytes  {}",
                            response.body.len(),
                            response.final_url
                        ),
                        Err(error) => {
                            println!("#{round} {elapsed:>9.1?} ERROR {error}  {url}");
                        }
                    }
                }
            }
        }
        other => eprintln!("unknown mode '{other}'"),
    }
}
