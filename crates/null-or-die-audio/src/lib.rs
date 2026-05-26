use std::fs::File;
use std::path::Path;
use std::process::Command;

use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::conv::IntoSample;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::sample::Sample;

const PCM_INV_SCALE: f32 = 1.0 / 32768.0;

#[derive(Debug, Clone)]
pub struct OggDecode {
    pub sample_rate_hz: u32,
    pub mono: Vec<f32>,
}

pub fn decode_ogg_mono_like_python(path: &Path) -> Result<OggDecode, String> {
    match decoder_pref().as_deref() {
        Some("ffmpeg") => {
            let (sample_rate_hz, source_channels) = probe_ogg_header(path)?;
            return decode_ogg_ffmpeg(path, sample_rate_hz, source_channels);
        }
        Some("symphonia") | Some("auto") | None => {}
        _ => {}
    }
    match decode_ogg_symphonia(path) {
        Ok(decoded) => Ok(decoded),
        Err(symphonia_err) => {
            let (sample_rate_hz, source_channels) = match probe_ogg_header(path) {
                Ok(v) => v,
                Err(probe_err) => {
                    return Err(format!(
                        "symphonia decode failed: {symphonia_err}; ffmpeg fallback could not probe header: {probe_err}"
                    ));
                }
            };
            match decode_ogg_ffmpeg(path, sample_rate_hz, source_channels) {
                Ok(decoded) => Ok(decoded),
                Err(ffmpeg_err) => Err(format!(
                    "symphonia decode failed: {symphonia_err}; fallback ffmpeg decode failed: {ffmpeg_err}"
                )),
            }
        }
    }
}

fn decoder_pref() -> Option<String> {
    std::env::var("NOD_AUDIO_DECODER")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
}

fn probe_ogg_header(path: &Path) -> Result<(u32, usize), String> {
    let file = File::open(path).map_err(|e| format!("open {} failed: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("ogg");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("ogg header parse {} failed: {e}", path.display()))?;
    let track = probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| format!("ogg {} has no usable audio track", path.display()))?;
    let sample_rate_hz = track
        .codec_params
        .sample_rate
        .ok_or_else(|| format!("ogg {} missing sample rate", path.display()))?;
    let source_channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .ok_or_else(|| format!("ogg {} missing channel layout", path.display()))?;
    Ok((sample_rate_hz, source_channels))
}

fn decode_ogg_ffmpeg(
    path: &Path,
    sample_rate_hz: u32,
    source_channels: usize,
) -> Result<OggDecode, String> {
    let output = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-i")
        .arg(path)
        .arg("-map")
        .arg("a:0")
        .arg("-f")
        .arg("s16le")
        .arg("-acodec")
        .arg("pcm_s16le")
        .arg("-")
        .output()
        .map_err(|e| format!("ffmpeg decode {} failed: {e}", path.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffmpeg decode {} failed: {}",
            path.display(),
            stderr.trim()
        ));
    }
    Ok(OggDecode {
        sample_rate_hz,
        mono: mono_from_interleaved_pcm_i16(&output.stdout, source_channels),
    })
}

fn decode_ogg_symphonia(path: &Path) -> Result<OggDecode, String> {
    let file = File::open(path).map_err(|e| format!("open {} failed: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("ogg");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("ogg probe {} failed: {e}", path.display()))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| format!("ogg {} has no usable audio track", path.display()))?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let sample_rate_hz = codec_params
        .sample_rate
        .ok_or_else(|| format!("ogg {} missing sample rate", path.display()))?;
    let source_channels = codec_params
        .channels
        .map(|c| c.count())
        .ok_or_else(|| format!("ogg {} missing channel layout", path.display()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| format!("ogg decoder init {} failed: {e}", path.display()))?;
    // Lewton retains the encoder-delay samples at the start of the stream and
    // trims only the end padding using the final granule position. With gapless
    // disabled, symphonia delivers every decoded frame (delay + content + end
    // padding), so we mirror lewton by trimming exactly `padding` frames off
    // the tail of the assembled mono buffer.
    let end_padding_frames = codec_params.padding.unwrap_or(0) as usize;
    let mut mono = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("ogg read {} failed: {e}", path.display())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let audio_buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("ogg decode {} failed: {e}", path.display())),
        };
        append_symphonia_buffer_python_mono_like(&audio_buf, source_channels, &mut mono);
    }
    if end_padding_frames > 0 && end_padding_frames <= mono.len() {
        mono.truncate(mono.len() - end_padding_frames);
    }
    Ok(OggDecode {
        sample_rate_hz,
        mono,
    })
}

fn append_symphonia_buffer_python_mono_like(
    buf: &AudioBufferRef<'_>,
    source_channels: usize,
    out: &mut Vec<f32>,
) {
    match buf {
        AudioBufferRef::U8(b) => append_planar_python_mono_like(b.as_ref(), source_channels, out),
        AudioBufferRef::U16(b) => append_planar_python_mono_like(b.as_ref(), source_channels, out),
        AudioBufferRef::U24(b) => append_planar_python_mono_like(b.as_ref(), source_channels, out),
        AudioBufferRef::U32(b) => append_planar_python_mono_like(b.as_ref(), source_channels, out),
        AudioBufferRef::S8(b) => append_planar_python_mono_like(b.as_ref(), source_channels, out),
        AudioBufferRef::S16(b) => append_planar_python_mono_like(b.as_ref(), source_channels, out),
        AudioBufferRef::S24(b) => append_planar_python_mono_like(b.as_ref(), source_channels, out),
        AudioBufferRef::S32(b) => append_planar_python_mono_like(b.as_ref(), source_channels, out),
        AudioBufferRef::F32(b) => append_planar_python_mono_like(b.as_ref(), source_channels, out),
        AudioBufferRef::F64(b) => append_planar_python_mono_like(b.as_ref(), source_channels, out),
    }
}

fn append_planar_python_mono_like<S>(
    buf: &symphonia::core::audio::AudioBuffer<S>,
    source_channels: usize,
    out: &mut Vec<f32>,
) where
    S: Sample + IntoSample<i16> + Copy,
{
    let spec_channels = buf.spec().channels.count();
    let channels = spec_channels.min(source_channels.max(1));
    let frames = buf.frames();
    if channels == 0 || frames == 0 {
        return;
    }
    let mut planes: Vec<Vec<i16>> = Vec::with_capacity(channels);
    for ch in 0..channels {
        let plane = buf.chan(ch);
        let mut converted: Vec<i16> = Vec::with_capacity(frames);
        for s in &plane[..frames] {
            converted.push((*s).into_sample());
        }
        planes.push(converted);
    }
    append_python_mono_like(&planes, source_channels, out);
}

fn mono_from_interleaved_pcm_i16(bytes: &[u8], channels: usize) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    if channels == 2 {
        let mut out = Vec::with_capacity(bytes.len() / 4);
        for frame in bytes.chunks_exact(4) {
            let left = i16::from_le_bytes([frame[0], frame[1]]);
            let right = i16::from_le_bytes([frame[2], frame[3]]);
            out.push(f32::from(left.max(right)) * PCM_INV_SCALE);
        }
        return out;
    }
    if channels == 1 {
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for sample in bytes.chunks_exact(2) {
            out.push(f32::from(i16::from_le_bytes([sample[0], sample[1]])) * PCM_INV_SCALE);
        }
        return out;
    }
    let frame_bytes = channels * 2;
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for frame in bytes.chunks_exact(frame_bytes) {
        for sample in frame.chunks_exact(2) {
            out.push(f32::from(i16::from_le_bytes([sample[0], sample[1]])) * PCM_INV_SCALE);
        }
    }
    out
}

fn append_python_mono_like(packet: &[Vec<i16>], channels: usize, out: &mut Vec<f32>) {
    if channels == 2 && packet.len() >= 2 {
        append_stereo_max(&packet[0], &packet[1], out);
    } else if packet.len() == 1 {
        append_passthrough(&packet[0], out);
    } else {
        append_interleaved_passthrough(packet, out);
    }
}

fn append_stereo_max(left: &[i16], right: &[i16], out: &mut Vec<f32>) {
    let len = left.len().min(right.len());
    out.reserve(len);
    let mut i = 0usize;
    while i < len {
        out.push(f32::from(left[i].max(right[i])) * PCM_INV_SCALE);
        i += 1;
    }
}

fn append_passthrough(packet: &[i16], out: &mut Vec<f32>) {
    out.reserve(packet.len());
    for s in packet {
        out.push(f32::from(*s) * PCM_INV_SCALE);
    }
}

fn append_interleaved_passthrough(packet: &[Vec<i16>], out: &mut Vec<f32>) {
    if packet.is_empty() {
        return;
    }
    let channels = packet.len();
    let frames = packet[0].len();
    out.reserve(channels * frames);
    let mut i = 0usize;
    while i < frames {
        for ch in packet {
            out.push(f32::from(ch[i]) * PCM_INV_SCALE);
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{append_python_mono_like, mono_from_interleaved_pcm_i16};

    #[test]
    fn stereo_collapse_uses_channel_max() {
        let mut out = Vec::new();
        append_python_mono_like(&[vec![100, -3200], vec![200, -6400]], 2, &mut out);
        assert_eq!(out.len(), 2);
        assert!((out[0] - (200.0 / 32768.0)).abs() < 1e-7);
        assert!((out[1] - (-3200.0 / 32768.0)).abs() < 1e-7);
    }

    #[test]
    fn mono_passthrough_is_normalized() {
        let mut out = Vec::new();
        append_python_mono_like(&[vec![32767, 0, -32768]], 1, &mut out);
        assert_eq!(out.len(), 3);
        assert!((out[0] - (32767.0 / 32768.0)).abs() < 1e-7);
        assert_eq!(out[1], 0.0);
        assert_eq!(out[2], -1.0);
    }

    #[test]
    fn multichannel_passthrough_stays_interleaved() {
        let mut out = Vec::new();
        append_python_mono_like(&[vec![1, 2], vec![10, 20], vec![100, 200]], 1, &mut out);
        assert_eq!(out.len(), 6);
        assert!((out[0] - (1.0 / 32768.0)).abs() < 1e-7);
        assert!((out[1] - (10.0 / 32768.0)).abs() < 1e-7);
        assert!((out[2] - (100.0 / 32768.0)).abs() < 1e-7);
        assert!((out[3] - (2.0 / 32768.0)).abs() < 1e-7);
        assert!((out[4] - (20.0 / 32768.0)).abs() < 1e-7);
        assert!((out[5] - (200.0 / 32768.0)).abs() < 1e-7);
    }

    #[test]
    fn ffmpeg_pcm_stereo_uses_channel_max() {
        let bytes = [100u8, 0, 200, 0, 0x80, 0xff, 0x00, 0x80];
        let out = mono_from_interleaved_pcm_i16(&bytes, 2);
        assert_eq!(out.len(), 2);
        assert!((out[0] - (200.0 / 32768.0)).abs() < 1e-7);
        assert!((out[1] - (-128.0 / 32768.0)).abs() < 1e-7);
    }
}
