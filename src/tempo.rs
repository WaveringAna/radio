use std::path::Path;

use anyhow::{Context, anyhow};
use tokio::process::Command;

/// Analysis sample rate. Tempo lives well below this; keeping it low makes the
/// decode and envelope cheap on the Pi.
const SAMPLE_RATE: u32 = 11025;
/// Samples per energy frame.
const FRAME: usize = 512;
/// Samples between frame starts; 11025 / 128 ≈ 86 envelope points per second,
/// enough lag resolution for ±1-2 BPM around typical tempos.
const HOP: usize = 128;
const ENVELOPE_RATE: f64 = SAMPLE_RATE as f64 / HOP as f64;
/// Search range. Anything slower or faster is reported at its octave inside
/// the range, which is what tempo pairing wants anyway.
const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 180.0;
/// Skip intros and fade-ins, then analyze a steady stretch.
const ANALYSIS_SKIP_SECS: &str = "15";
const ANALYSIS_LENGTH_SECS: &str = "90";
/// Autocorrelation peak must stand this far above the lag-range mean before
/// the estimate is trusted; below it we rather store nothing than noise.
const MIN_PEAK_RATIO: f64 = 1.25;

/// Estimates a track's tempo by autocorrelating its onset-energy envelope.
/// Returns `None` when the track has no confident periodicity (ambient,
/// rubato, spoken word).
pub(crate) async fn measure(path: &Path) -> anyhow::Result<Option<f64>> {
    let samples = decode_mono(path).await?;
    Ok(estimate_bpm(&samples))
}

/// Decodes the analysis window to mono f32 PCM via ffmpeg, matching the
/// loudness scanner's approach of shelling out rather than linking decoders.
async fn decode_mono(path: &Path) -> anyhow::Result<Vec<f32>> {
    let output = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-nostats",
            "-ss",
            ANALYSIS_SKIP_SECS,
            "-i",
        ])
        .arg(path)
        .args([
            "-t",
            ANALYSIS_LENGTH_SECS,
            "-map",
            "a:0",
            "-ac",
            "1",
            "-ar",
            &SAMPLE_RATE.to_string(),
            "-f",
            "f32le",
            "-",
        ])
        .output()
        .await
        .with_context(|| format!("running ffmpeg for {}", path.display()))?;

    if !output.status.success() {
        return Err(anyhow!(
            "ffmpeg failed for {} ({}): {}",
            path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let bytes = output.stdout;
    let mut samples = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(samples)
}

fn estimate_bpm(samples: &[f32]) -> Option<f64> {
    let envelope = onset_envelope(samples);
    // Ten seconds of envelope is the floor for a stable autocorrelation.
    if (envelope.len() as f64) < ENVELOPE_RATE * 10.0 {
        return None;
    }

    let min_lag = (60.0 / MAX_BPM * ENVELOPE_RATE).floor() as usize;
    let max_lag = (60.0 / MIN_BPM * ENVELOPE_RATE).ceil() as usize;
    if max_lag * 2 >= envelope.len() {
        return None;
    }

    let scores: Vec<f64> = (min_lag..=max_lag)
        .map(|lag| autocorrelation(&envelope, lag))
        .collect();
    let mean = scores.iter().sum::<f64>() / scores.len() as f64;
    if mean <= 0.0 {
        return None;
    }

    // Score each lag together with its half-tempo harmonic so a strong
    // off-beat doesn't win over the true pulse.
    let best_index = (0..scores.len()).max_by(|&a, &b| {
        let score = |i: usize| {
            let lag = min_lag + i;
            let harmonic = if lag * 2 <= max_lag {
                autocorrelation(&envelope, lag * 2)
            } else {
                0.0
            };
            scores[i] + 0.5 * harmonic
        };
        score(a).total_cmp(&score(b))
    })?;
    if scores[best_index] / mean < MIN_PEAK_RATIO {
        return None;
    }

    // Parabolic interpolation around the peak refines the lag below the
    // envelope's frame resolution.
    let lag = min_lag + best_index;
    let refined = if best_index > 0 && best_index + 1 < scores.len() {
        let (left, center, right) = (
            scores[best_index - 1],
            scores[best_index],
            scores[best_index + 1],
        );
        let denominator = left - 2.0 * center + right;
        if denominator.abs() > f64::EPSILON {
            lag as f64 + 0.5 * (left - right) / denominator
        } else {
            lag as f64
        }
    } else {
        lag as f64
    };

    let bpm = 60.0 * ENVELOPE_RATE / refined;
    // Fold extreme octaves into the common dance/rock band for stable pairing.
    let folded = if bpm < 70.0 {
        bpm * 2.0
    } else if bpm > 170.0 {
        bpm / 2.0
    } else {
        bpm
    };
    Some((folded * 10.0).round() / 10.0)
}

/// Half-wave-rectified frame-energy flux: rises in energy mark onsets, and a
/// light smoothing pass keeps single-sample spikes from dominating.
fn onset_envelope(samples: &[f32]) -> Vec<f64> {
    if samples.len() < FRAME {
        return Vec::new();
    }
    let mut energies = Vec::with_capacity(samples.len() / HOP);
    let mut start = 0;
    while start + FRAME <= samples.len() {
        let frame = &samples[start..start + FRAME];
        energies.push(frame.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>());
        start += HOP;
    }

    let mut flux: Vec<f64> = energies
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).max(0.0))
        .collect();
    for i in 1..flux.len() {
        flux[i] = 0.75 * flux[i] + 0.25 * flux[i - 1];
    }
    let mean = flux.iter().sum::<f64>() / flux.len().max(1) as f64;
    if mean > 0.0 {
        for value in &mut flux {
            *value /= mean;
        }
    }
    flux
}

fn autocorrelation(envelope: &[f64], lag: usize) -> f64 {
    let n = envelope.len() - lag;
    let mut sum = 0.0;
    for i in 0..n {
        sum += envelope[i] * envelope[i + lag];
    }
    sum / n as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesizes clicks at the given tempo with light noise.
    fn click_track(bpm: f64, seconds: f64) -> Vec<f32> {
        let total = (SAMPLE_RATE as f64 * seconds) as usize;
        let period = (SAMPLE_RATE as f64 * 60.0 / bpm) as usize;
        let mut seed = 0x2545f4914f6cdd1du64;
        (0..total)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let noise = ((seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.02;
                let click = if i % period < 256 { 0.9 } else { 0.0 };
                (click + noise) as f32
            })
            .collect()
    }

    #[test]
    fn detects_click_track_tempo() {
        for bpm in [80.0, 120.0, 150.0] {
            let estimate = estimate_bpm(&click_track(bpm, 30.0)).expect("confident estimate");
            assert!(
                (estimate - bpm).abs() < 3.0,
                "expected ≈{bpm}, got {estimate}"
            );
        }
    }

    #[test]
    fn rejects_noise() {
        let mut seed = 0x9e3779b97f4a7c15u64;
        let noise: Vec<f32> = (0..SAMPLE_RATE as usize * 30)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                ((seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5) as f32
            })
            .collect();
        assert_eq!(estimate_bpm(&noise), None);
    }

    #[test]
    fn rejects_short_input() {
        assert_eq!(estimate_bpm(&click_track(120.0, 3.0)), None);
    }
}
