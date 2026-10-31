//! Test-only managed peer for Chromium loopback qualification.
use amaterasu_webrtc_transport::{BrowserSessionSet, OwnerContext, SessionEvent};
use std::io::{self, BufRead, Write};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let owner = OwnerContext::from_verified_id(&"a".repeat(64))?;
    let mut sessions = BrowserSessionSet::new(Ipv4Addr::LOCALHOST.into(), false)?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let offer: serde_json::Value = serde_json::from_str(&line)?;
    let sdp = offer["sdp"].as_str().ok_or("missing offer SDP")?;
    let opened = sessions.open(&owner, "browser-test", sdp, Instant::now())?;
    println!(
        "{}",
        serde_json::json!({"type":"answer","sdp":opened.answer_sdp})
    );
    io::stdout().flush()?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        sessions.tick(&owner, &opened.id, Instant::now())?;
        while let Some(event) = sessions.next_event(&owner, &opened.id)? {
            if let SessionEvent::Input { bytes, .. } = event
                && bytes == b"probe-input"
            {
                eprintln!("input received");
                sessions.close(&owner, &opened.id)?;
                if sessions.state(&owner, &opened.id).is_ok() {
                    return Err("session remained after close".into());
                }
                eprintln!("session closed");
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Err("session did not receive browser input".into())
}
