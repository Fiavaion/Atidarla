//! The reference AI Host (see `lsr_ai::refhost`): built-in models only, for
//! tests and CI.

use std::net::TcpListener;
use std::time::Duration;

use lsr_ai::refhost::{RefHostOptions, serve, watch_parent};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut listen = "127.0.0.1:0".to_owned();
    let mut opts = RefHostOptions::default();
    while let Some(a) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage(&a));
        match a.as_str() {
            "--listen" => listen = value(),
            "--token" => opts.token = Some(value()),
            "--fake-vendor" => opts.vendor = Some(value()),
            "--delay-ms" => {
                opts.delay = Duration::from_millis(value().parse().unwrap_or_else(|_| usage(&a)));
            }
            "--parent-pid" => watch_parent(value().parse().unwrap_or_else(|_| usage(&a))),
            _ => usage(&a),
        }
    }
    if opts.token.is_none() {
        opts.token = std::env::var("LSR_AI_HOST_TOKEN").ok();
    }
    let listener = TcpListener::bind(&listen).unwrap_or_else(|e| {
        eprintln!("lsr-ai-refhost: cannot listen on {listen}: {e}");
        std::process::exit(2)
    });
    let addr = listener.local_addr().expect("bound address");
    if !addr.ip().is_loopback() && opts.token.is_none() {
        eprintln!("lsr-ai-refhost: a token is required to listen beyond loopback");
        std::process::exit(2);
    }
    println!("lsr-ai-host listening on {addr}");
    serve(listener, opts);
}

fn usage(arg: &str) -> ! {
    eprintln!(
        "lsr-ai-refhost: bad argument {arg:?}\nusage: lsr-ai-refhost [--listen ADDR:PORT] [--token T] [--parent-pid PID] [--fake-vendor V] [--delay-ms N]"
    );
    std::process::exit(2)
}
