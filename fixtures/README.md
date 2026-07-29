# Sotto deterministic fixture corpus

All audio here is programmatically synthesised and contains no recording of a real
person. The WAVs are 16 kHz, mono, 16-bit PCM and exactly 30 seconds long. Tone bursts
stand in for labelled speaking turns: these fixtures test capture clocks, stream
separation, VAD, replay, interruption geometry, and latency without licensing or consent
concerns. Whisper quality tests should use the separately owned ASR fixtures because
these tones intentionally contain no human speech.

`call-01-mic.wav` and `call-01-sys.wav` are the canonical paired call. Its timestamped
frames change from an overview slide to a pricing slide at 15 seconds. Expected labels
are recorded in `call-01-ground-truth.json`, while the generated high-contrast bands make
change detection deterministic. These synthetic frames currently contain no rendered
glyphs, so they do not constitute an Apple Vision accuracy fixture; the macOS CLI invokes
Vision best-effort and preserves the snapshot with empty OCR text if Vision rejects it.

The remaining cases are: objection/competitor, heavy crosstalk, long silence,
silence-only, and music-only. Regenerate binary assets deliberately with:

```sh
cargo run -p cli --example generate_fixtures
```

The reference timeline is stable JSONL intended for downstream replay tests. A diff is
a contract signal and should be reviewed.
