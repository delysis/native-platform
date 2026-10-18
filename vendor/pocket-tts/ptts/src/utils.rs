//! Loudness normalization for voice prompts: [`normalize_loudness`] brings a recording to a
//! fixed level before the speaker encoder embeds it.

use xn::Result;

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoicePromptPreprocessing {
    #[default]
    Loudness,
    PocketPause,
}

/// Pocket TTS 3.3 voice-prompt admission at the model's sample rate. Trim quiet
/// trailing 20 ms frames, fade the final 20 ms, then append exactly 80 ms silence.
/// Keep the recording's gain: loudness normalization is a different model policy.
pub fn pocket_voice_prompt(pcm: &[f32], sample_rate: u32) -> Result<Vec<f32>> {
    if sample_rate == 0 || pcm.iter().any(|sample| !sample.is_finite()) {
        xn::bail!("voice prompt requires finite mono PCM and a nonzero sample rate")
    }
    let frame = (sample_rate as usize / 50).max(1);
    let n = pcm.len() / frame;
    if n == 0 {
        return Ok(pcm.to_vec());
    }
    let db: Vec<f32> = pcm[..n * frame]
        .chunks_exact(frame)
        .map(|samples| {
            let mean = samples.iter().map(|x| x * x).sum::<f32>() / frame as f32;
            20.0 * (mean.sqrt() + 1e-12).log10()
        })
        .collect();
    let max_db = db.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let last_loud = db
        .iter()
        .rposition(|level| *level > max_db - 35.0)
        .ok_or_else(|| xn::Error::msg("voice prompt has no finite energy frames"))?;
    let end = (last_loud + 1) * frame;
    let mut out = pcm[..end].to_vec();
    let fade = (sample_rate as usize / 50).min(end);
    if fade > 1 {
        for (index, sample) in out[end - fade..].iter_mut().enumerate() {
            *sample *= 1.0 - index as f32 / (fade - 1) as f32;
        }
    }
    out.resize(end + sample_rate as usize * 8 / 100, 0.0);
    Ok(out)
}

#[cfg(test)]
mod pocket_tests {
    use super::*;

    #[test]
    fn trailing_quiet_frames_are_replaced_without_changing_voice_gain() {
        let mut pcm = vec![0.25; 960];
        pcm.extend(vec![0.00001; 1440]);
        let out = pocket_voice_prompt(&pcm, 24_000).unwrap();
        assert_eq!(out.len(), 960 + 1920);
        assert!(out[..480].iter().all(|s| *s == 0.25));
        assert_eq!(out[480], 0.25);
        assert_eq!(out[959], 0.0);
        assert!(out[960..].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn partial_frame_and_silent_prompt_follow_reference_bounds() {
        assert_eq!(pocket_voice_prompt(&[0.1; 100], 24_000).unwrap(), vec![0.1; 100]);
        assert_eq!(pocket_voice_prompt(&[0.0; 1000], 24_000).unwrap(), vec![0.0; 2880]);
        assert!(pocket_voice_prompt(&[f32::NAN], 24_000).is_err());
    }
}

/// Target loudness (in dB LUFS) used when normalizing voice-embedding input.
/// Mirrors `loudness_headroom_db` in `compute-engine/worker/src/voice.py`.
pub const LOUDNESS_HEADROOM_DB: f64 = 18.0;
/// RMS level below which the signal is considered too quiet to rescale.
pub const LOUDNESS_ENERGY_FLOOR: f32 = 2e-3;

/// Normalize `pcm` to a target loudness (in dB LUFS) following ITU-R BS.1770-4.
///
/// This mirrors `normalize_loudness` in `compute-engine/worker/src/voice.py`.
/// The signal is first centered (DC removed), then skipped entirely if its RMS
/// is below [`LOUDNESS_ENERGY_FLOOR`], otherwise rescaled so that its
/// integrated loudness equals `-LOUDNESS_HEADROOM_DB` LUFS.
pub fn normalize_loudness(pcm: &mut [f32], sample_rate: u32) -> Result<()> {
    if pcm.is_empty() {
        return Ok(());
    }
    // Remove DC offset.
    let mean = (pcm.iter().map(|&s| s as f64).sum::<f64>() / pcm.len() as f64) as f32;
    for s in pcm.iter_mut() {
        *s -= mean;
    }
    // Skip rescaling for signals below the energy floor (matches the Python
    // version which returns the input unchanged in that case).
    let rms = (pcm.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / pcm.len() as f64).sqrt()
        as f32;
    if rms < LOUDNESS_ENERGY_FLOOR {
        return Ok(());
    }
    let mut meter =
        ebur128::EbuR128::new(1, sample_rate, ebur128::Mode::I).map_err(xn::Error::wrap)?;
    meter.add_frames_f32(pcm).map_err(xn::Error::wrap)?;
    let input_loudness_db = match meter.loudness_global() {
        Ok(l) if l.is_finite() => l,
        // Audio too short or silent — leave it unchanged, matching the Python
        // fallback where `torchaudio.transforms.Loudness` raises `RuntimeError`.
        _ => return Ok(()),
    };
    let delta_loudness = -LOUDNESS_HEADROOM_DB - input_loudness_db;
    let gain = 10f64.powf(delta_loudness / 20.0) as f32;
    if !gain.is_finite() {
        return Ok(());
    }
    for s in pcm.iter_mut() {
        *s *= gain;
    }
    Ok(())
}
