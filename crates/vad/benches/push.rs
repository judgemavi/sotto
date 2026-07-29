use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use criterion::{Criterion, criterion_group, criterion_main};
use sotto_core::{AudioFrame, Source, VoiceActivityDetector};
use vad::{SileroVad, VadConfig};

fn push_frame(criterion: &mut Criterion) {
    let Ok(mut vad) = SileroVad::new(Source::Mic, VadConfig::default()) else {
        return;
    };
    let frame = AudioFrame {
        source: Source::Mic,
        samples: Arc::from([0.0; 512]),
        sample_rate: 16_000,
        seq: 0,
        capture_ts: Instant::now(),
        stream_offset: Duration::ZERO,
    };
    criterion.bench_function("silero_512_samples", |bencher| {
        bencher.iter(|| vad.push(&frame));
    });
}

criterion_group!(benches, push_frame);
criterion_main!(benches);
