use std::{f32::consts::TAU, fs, path::Path};

const RATE: u32 = 16_000;
const SECONDS: u32 = 30;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new("fixtures");
    fs::create_dir_all(root.join("call-01-frames"))?;
    fs::create_dir_all(root.join("timelines"))?;
    write_wav(
        &root.join("call-01-mic.wav"),
        &[(1, 5, 185.0), (13, 17, 205.0), (24, 28, 195.0)],
    )?;
    write_wav(
        &root.join("call-01-sys.wav"),
        &[(6, 12, 225.0), (18, 23, 235.0)],
    )?;
    write_wav(
        &root.join("objection-mic.wav"),
        &[(1, 7, 190.0), (18, 26, 200.0)],
    )?;
    write_wav(&root.join("objection-system.wav"), &[(8, 18, 230.0)])?;
    write_wav(
        &root.join("crosstalk-mic.wav"),
        &[(2, 14, 180.0), (19, 27, 200.0)],
    )?;
    write_wav(&root.join("crosstalk-system.wav"), &[(8, 20, 240.0)])?;
    write_wav(
        &root.join("long-silence.wav"),
        &[(1, 4, 210.0), (25, 29, 220.0)],
    )?;
    write_wav(&root.join("silence.wav"), &[])?;
    write_wav(&root.join("music.wav"), &[(0, 30, 440.0)])?;
    write_png(
        &root.join("call-01-frames/00000-overview.png"),
        [30, 45, 60, 255],
    )?;
    write_png(
        &root.join("call-01-frames/15000-pricing.png"),
        [230, 245, 255, 255],
    )?;
    Ok(())
}

fn write_wav(path: &Path, intervals: &[(u32, u32, f32)]) -> Result<(), hound::Error> {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )?;
    for sample_index in 0..RATE * SECONDS {
        let second = sample_index / RATE;
        let value = intervals
            .iter()
            .find(|(start, end, _)| second >= *start && second < *end)
            .map_or(0.0, |(_, _, frequency)| {
                let envelope = ((sample_index % RATE) as f32 / RATE as f32 * std::f32::consts::PI)
                    .sin()
                    .abs();
                (sample_index as f32 / RATE as f32 * *frequency * TAU).sin() * 0.18 * envelope
            });
        writer.write_sample(value)?;
    }
    writer.finalize()
}

fn write_png(path: &Path, rgba: [u8; 4]) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = (320, 180);
    let file = fs::File::create(path)?;
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let mut pixels = rgba.repeat(width as usize * height as usize);
    // High-contrast bands make slide changes deterministic for perceptual hashing.
    for y in 70..110_usize {
        for x in 35..285_usize {
            let index = (y * width as usize + x) * 4;
            pixels[index..index + 4].copy_from_slice(&[
                rgba[0] ^ 0xff,
                rgba[1] ^ 0xff,
                rgba[2] ^ 0xff,
                255,
            ]);
        }
    }
    writer.write_image_data(&pixels)?;
    writer.finish()?;
    Ok(())
}
