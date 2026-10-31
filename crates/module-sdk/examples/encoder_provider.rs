//! A source encoder can publish owned output without submitting input frames.
use amaterasu_module_sdk::encode::{EncodedPacket, FormatId, OutputQueue, SessionMode};

fn main() {
    let output = OutputQueue::new(SessionMode::Source, 8, 1024 * 1024).unwrap();
    let packet = EncodedPacket {
        flags: 0,
        session_id: 1,
        access_unit_id: 1,
        submission_id: 0,
        source_id: 1,
        config_generation: 1,
        timestamp_us: 0,
        codec: FormatId::default(),
        framing: FormatId::default(),
        bytes: vec![1, 2, 3],
    };
    output.push(packet).unwrap();
    let front = output.front().unwrap();
    // Invoke the consumer sink here; only acknowledge after it accepts.
    assert_eq!(front.bytes, [1, 2, 3]);
    output.acknowledge(&front).unwrap();
}
