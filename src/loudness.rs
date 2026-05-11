use std::path::Path;

use anyhow::{Context, anyhow};
use tokio::process::Command;

#[derive(Clone, Copy, Debug)]
pub(crate) struct LoudnessMeasurement {
    pub(crate) integrated_lufs: f64,
    pub(crate) true_peak_dbfs: f64,
}

/// Measures integrated loudness (LUFS) and true peak (dBFS) for an audio file
/// by running `ffmpeg` with the `ebur128` filter.
pub(crate) async fn measure(path: &Path) -> anyhow::Result<LoudnessMeasurement> {
    let output = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-nostats",
            "-i",
        ])
        .arg(path)
        .args(["-map", "a:0", "-af", "ebur128=peak=true", "-f", "null", "-"])
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    let summary_start = stderr
        .rfind("Summary:")
        .ok_or_else(|| anyhow!("ffmpeg ebur128 output missing summary"))?;
    let summary = &stderr[summary_start..];

    let integrated_lufs = parse_field(summary, "I:", "LUFS")
        .ok_or_else(|| anyhow!("could not parse integrated loudness for {}", path.display()))?;
    let true_peak_dbfs = parse_field(summary, "Peak:", "dBFS")
        .ok_or_else(|| anyhow!("could not parse true peak for {}", path.display()))?;

    Ok(LoudnessMeasurement {
        integrated_lufs,
        true_peak_dbfs,
    })
}

fn parse_field(haystack: &str, label: &str, unit: &str) -> Option<f64> {
    let label_idx = haystack.find(label)?;
    let after = &haystack[label_idx + label.len()..];
    let unit_idx = after.find(unit)?;
    after[..unit_idx].trim().parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_summary_block() {
        let sample = "Summary:\n\n  Integrated loudness:\n    I:         -14.5 LUFS\n    Threshold: -24.7 LUFS\n\n  True peak:\n    Peak:       -1.4 dBFS\n";
        assert_eq!(parse_field(sample, "I:", "LUFS"), Some(-14.5));
        assert_eq!(parse_field(sample, "Peak:", "dBFS"), Some(-1.4));
    }
}
