//! Lookup-free executable fixtures for testing the production child launcher.
//! This executable is never embedded by a production provider build.

#[allow(dead_code)]
#[path = "helper_main.rs"]
mod production;

use std::ffi::OsString;
use std::io::{self, Write};

use production::helperprotocol::{MAX_RESPONSE_BYTES, Request, Response};

fn fixture_mode(mut arguments: Vec<OsString>) -> io::Result<String> {
    let mode_index = arguments
        .iter()
        .position(|argument| {
            matches!(
                argument.to_str(),
                Some("block" | "malformed" | "oversized" | "addresses_then_block")
            )
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "fixture mode required"))?;
    let mode = arguments.remove(mode_index).into_string().unwrap();
    let parent = production::parent_process_id(arguments)?;
    production::prepare_parent_lifetime(parent)?;
    Ok(mode)
}

fn run() -> io::Result<()> {
    let mode = fixture_mode(std::env::args_os().skip(1).collect())?;
    let request = Request::read_from(io::stdin().lock())?;
    let mut output = io::stdout().lock();
    match mode.as_str() {
        "block" => park(),
        "malformed" => output.write_all(b"invalid resolver response"),
        "oversized" => output.write_all(&vec![0; MAX_RESPONSE_BYTES + 1]),
        "addresses_then_block" => {
            Response::Addresses(vec![std::net::SocketAddr::new(
                std::net::Ipv4Addr::LOCALHOST.into(),
                request.port,
            )])
            .write_to(&mut output)?;
            output.flush()?;
            park()
        }
        _ => unreachable!("fixture mode was validated"),
    }
}

fn park() -> ! {
    loop {
        std::thread::park();
    }
}

fn main() {
    if run().is_err() {
        std::process::exit(1);
    }
}
