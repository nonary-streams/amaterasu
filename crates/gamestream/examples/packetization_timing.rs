use amaterasu_gamestream::video::{VideoCodec, VideoConfig, VideoPacketizer};
use std::{hint::black_box, time::Instant};

fn main() {
    for bytes in [8_000, 48_000, 96_000, 240_000, 480_000] {
        let mut frame = vec![0x55; bytes];
        frame[..7].copy_from_slice(&[0, 0, 0, 1, 2, 1, 0x80]);
        let mut packetizer = VideoPacketizer::new_with_codec(
            VideoConfig {
                packet_size: 1392,
                fec_percentage: 20,
                min_parity_shards: 2,
                max_fec_blocks: 4,
                encrypted: false,
            },
            VideoCodec::Hevc,
        )
        .unwrap();
        let started = Instant::now();
        for index in 0..300 {
            black_box(
                packetizer
                    .packetize(black_box(&frame), false, index, index * 750)
                    .unwrap(),
            );
        }
        println!(
            "{bytes} bytes: {:.1} us/frame",
            started.elapsed().as_secs_f64() * 1e6 / 300.0
        );
    }
}
