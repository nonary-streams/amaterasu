//! Test-only loopback peer used by the Chromium qualification script.
use amaterasu_webrtc_transport::BrowserTransport;
use std::io::{self, BufRead, Write};
use std::net::UdpSocket;
use std::time::{Duration, Instant};
use str0m::format::Codec;
use str0m::{Event, Output};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = UdpSocket::bind("127.0.0.1:0")?;
    let mut peer = BrowserTransport::new(socket.local_addr()?, Instant::now())?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let offer: serde_json::Value = serde_json::from_str(&line)?;
    let sdp = offer["sdp"].as_str().ok_or("missing offer SDP")?;
    let answer = peer.accept_offer(sdp)?;
    println!("{}", serde_json::json!({"type":"answer","sdp":answer}));
    io::stdout().flush()?;
    let frame = include_bytes!("../tests/fixtures/test-pattern-128-baseline.h264");
    let start = Instant::now();
    let mut next_video = start;
    let mut next_audio = start;
    let mut frames = 0u64;
    let mut packets = 0u64;
    let mut checked_format = false;
    let mut buffer = [0u8; 4096];
    while start.elapsed() < Duration::from_secs(15) {
        let deadline = loop {
            match peer.poll_output()? {
                Output::Timeout(deadline) => break deadline,
                Output::Transmit(packet) => {
                    socket.send_to(&packet.contents, packet.destination)?;
                }
                Output::Event(Event::ChannelData(data)) if data.data == b"probe-input" => {
                    eprintln!("input received")
                }
                Output::Event(_) => {}
            }
        };
        let now = Instant::now();
        if peer.is_connected() && !checked_format {
            let format = peer.selected_h264_format()?;
            if format.codec != Codec::H264 || format.format.packetization_mode != Some(1) {
                return Err("H.264 was not negotiated".into());
            }
            checked_format = true;
        }
        if peer.is_connected() && now >= next_video {
            peer.send_h264(now, frames * 33_333, frame)?;
            frames += 1;
            next_video = now + Duration::from_millis(33);
            continue;
        }
        if peer.is_connected() && now >= next_audio {
            peer.send_opus(now, packets * 20_000, &[0xf8, 0xff, 0xfe])?;
            packets += 1;
            next_audio = now + Duration::from_millis(20);
            continue;
        }
        let wait = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(5))
            .max(Duration::from_micros(1));
        socket.set_read_timeout(Some(wait))?;
        match socket.recv_from(&mut buffer) {
            Ok((length, source)) => {
                peer.receive_datagram(Instant::now(), source, &buffer[..length])?
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                peer.handle_timeout(Instant::now())?
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
