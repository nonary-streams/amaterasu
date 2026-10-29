//! Run with `cargo run -p amaterasu-module-sdk --example capture_notifications`.
//! Capturing and consuming stay independent; this example performs no encoding.
use amaterasu_module_sdk::{
    capture::{CaptureError, SwapDecision, SwapRequest, capture_channel},
    media::OwnedCpuFrame,
    media_abi::PIXEL_BGRA8,
};
use std::time::Duration;

fn main() {
    let (publisher, frames) = capture_channel();
    let notifications = frames.notifications();
    let worker = std::thread::spawn(move || {
        // Replace this allocation with your actual capture method. GPU providers
        // publish their own immutable resource owner instead of OwnedCpuFrame.
        let frame = OwnedCpuFrame::new(2, 2, PIXEL_BGRA8, 8, vec![128; 16], 100, 1).unwrap();
        publisher
            .publish(frame)
            .map_err(|error| error.error)
            .unwrap();
        let request = SwapRequest {
            preferred_provider: Some("other_capture".into()),
            output: Some("matching-output".into()),
            reason: "source_unavailable".into(),
        };
        let id = loop {
            match publisher.request_capture_swap(request.clone()) {
                Ok(id) => break id,
                Err(CaptureError::Busy) => std::thread::yield_now(),
                Err(error) => panic!("{error}"),
            }
        };
        // The producer can keep capturing while the coordinator decides.
        assert!(matches!(
            publisher.swap_decision(id),
            Ok(_) | Err(CaptureError::Closed)
        ));
    });
    let mut saw_frame = false;
    let mut saw_swap = false;
    while !saw_frame || !saw_swap {
        let notice = notifications.wait_timeout(Duration::from_secs(2));
        assert!(notice.frame_ready || notice.swap_request.is_some());
        if notice.frame_ready {
            let frame = frames.latest_frame().unwrap().unwrap();
            println!("Captured {} bytes", frame.pixels().len());
            saw_frame = true;
        }
        if let Some(swap) = notice.swap_request {
            // A real coordinator validates its provider policy and output map.
            frames
                .resolve_swap(
                    swap.id,
                    SwapDecision::Rejected {
                        reason: "example_only".into(),
                    },
                )
                .unwrap();
            saw_swap = true;
        }
    }
    worker.join().unwrap();
    frames.close();
}
