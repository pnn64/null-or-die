use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use symphonia::core::codecs::audio::{
    AudioDecoder, AudioDecoderOptions, well_known::CODEC_ID_VORBIS,
};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Duration;

const PCM_INV_SCALE: f32 = 1.0 / 32768.0;
const OGG_PAGE_HEADER_LEN: usize = 27;
const LEGACY_GRANULE_PROBE_LEN: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct OggDecode {
    pub sample_rate_hz: u32,
    pub mono: Vec<f32>,
}

pub fn decode_ogg_mono_like_python(path: &Path) -> Result<OggDecode, String> {
    match decoder_pref().as_deref() {
        Some("symphonia") | None => decode_ogg_symphonia(path),
        Some("ffmpeg") => decode_ogg_ffmpeg(path),
        _ => decode_ogg_ffmpeg(path).or_else(|ffmpeg_err| {
            decode_ogg_symphonia(path).map_err(|symphonia_err| {
                format!("{ffmpeg_err}; fallback symphonia decode failed: {symphonia_err}")
            })
        }),
    }
}

fn decoder_pref() -> Option<String> {
    std::env::var("NOD_AUDIO_DECODER")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
}

struct OggReader {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    sample_rate_hz: u32,
    channels: usize,
}

fn open_ogg(path: &Path) -> Result<OggReader, String> {
    let file = File::open(path).map_err(|e| format!("open {} failed: {e}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension("ogg");
    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("ogg header parse {} failed: {e}", path.display()))?;
    let (track, params) = format
        .tracks()
        .iter()
        .find_map(|track| {
            let params = track.codec_params.as_ref()?.audio()?;
            (params.codec == CODEC_ID_VORBIS).then_some((track, params))
        })
        .ok_or_else(|| format!("ogg {} has no Vorbis track", path.display()))?;
    let track_id = track.id;
    let channels = params
        .channels
        .as_ref()
        .map_or(0, |channels| channels.count());
    if channels == 0 {
        return Err(format!("ogg {} has unknown channel layout", path.display()));
    }
    let sample_rate_hz = params
        .sample_rate
        .ok_or_else(|| format!("ogg {} has unknown sample rate", path.display()))?;
    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("create Vorbis decoder for {} failed: {e}", path.display()))?;
    Ok(OggReader {
        format,
        decoder,
        track_id,
        sample_rate_hz,
        channels,
    })
}

fn decode_ogg_ffmpeg(path: &Path) -> Result<OggDecode, String> {
    let reader = open_ogg(path)?;
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
        sample_rate_hz: reader.sample_rate_hz,
        mono: mono_from_interleaved_pcm_i16(&output.stdout, reader.channels),
    })
}

fn decode_ogg_symphonia(path: &Path) -> Result<OggDecode, String> {
    let legacy_missing_granules = has_legacy_missing_granules_in_prefix(path)
        .map_err(|e| format!("read ogg {} failed: {e}", path.display()))?;
    let mut reader = open_ogg(path)?;
    let mut pcm = Vec::new();
    let mut mono = Vec::new();
    loop {
        let mut packet = match reader.format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) | Err(SymphoniaError::ResetRequired) => break,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(e) => return Err(format!("ogg read {} failed: {e}", path.display())),
        };
        if packet.track_id != reader.track_id {
            continue;
        }
        // Match deadsync's handling of old encoders that omit granules on
        // completed audio pages: Symphonia otherwise trims valid PCM away.
        if legacy_missing_granules
            && packet.dur == Duration::ZERO
            && packet.trim_end > Duration::ZERO
        {
            packet.dur = packet.trim_end;
            packet.trim_end = Duration::ZERO;
        }
        let audio = match reader.decoder.decode(&packet) {
            Ok(audio) => audio,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("ogg decode {} failed: {e}", path.display())),
        };
        if audio.frames() == 0 {
            continue;
        }
        audio.copy_to_vec_interleaved::<i16>(&mut pcm);
        append_python_mono_like(&pcm, reader.channels, &mut mono);
    }
    if mono.is_empty() {
        return Err(format!(
            "ogg {} contained no decodable audio frames",
            path.display()
        ));
    }
    Ok(OggDecode {
        sample_rate_hz: reader.sample_rate_hz,
        mono,
    })
}

// Keep this bounded, as in deadsync: affected legacy encoders expose the
// missing-granule layout near the start of the stream.
fn has_legacy_missing_granules_in_prefix(path: &Path) -> std::io::Result<bool> {
    let mut prefix = Vec::with_capacity(LEGACY_GRANULE_PROBE_LEN);
    File::open(path)?
        .take(LEGACY_GRANULE_PROBE_LEN as u64)
        .read_to_end(&mut prefix)?;
    let mut page_start = 0_usize;
    loop {
        let Some(header_end) = page_start.checked_add(OGG_PAGE_HEADER_LEN) else {
            return Ok(false);
        };
        let Some(header) = prefix.get(page_start..header_end) else {
            return Ok(false);
        };
        if &header[..4] != b"OggS" || header[4] != 0 {
            return Ok(false);
        }
        let segment_count = usize::from(header[26]);
        let Some(segment_end) = header_end.checked_add(segment_count) else {
            return Ok(false);
        };
        let Some(segments) = prefix.get(header_end..segment_end) else {
            return Ok(false);
        };
        if header[6..14].iter().all(|&byte| byte == u8::MAX)
            && segments.iter().any(|&length| length < u8::MAX)
        {
            return Ok(true);
        }
        let body_len = segments.iter().map(|&length| usize::from(length)).sum();
        let Some(next_page) = segment_end.checked_add(body_len) else {
            return Ok(false);
        };
        if next_page > prefix.len() {
            return Ok(false);
        }
        page_start = next_page;
    }
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

fn append_python_mono_like(packet: &[i16], channels: usize, out: &mut Vec<f32>) {
    if channels == 2 {
        out.extend(
            packet
                .chunks_exact(2)
                .map(|frame| f32::from(frame[0].max(frame[1])) * PCM_INV_SCALE),
        );
    } else {
        out.extend(
            packet
                .iter()
                .map(|&sample| f32::from(sample) * PCM_INV_SCALE),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        OGG_PAGE_HEADER_LEN, append_python_mono_like, decode_ogg_symphonia,
        has_legacy_missing_granules_in_prefix, mono_from_interleaved_pcm_i16,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use symphonia::core::checksum::Crc32;
    use symphonia::core::io::Monitor;

    static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

    struct TempOgg(PathBuf);

    impl TempOgg {
        fn new(bytes: &[u8]) -> Self {
            let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("null-or-die-audio-{}-{id}.ogg", std::process::id()));
            fs::write(&path, bytes).expect("write OGG fixture");
            Self(path)
        }
    }

    impl Drop for TempOgg {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    // FNV-1a over little-endian PCM16 after null-or-die's channel collapse.
    // Expected hashes were verified against deadsync's OGG reader.
    fn pcm_hash(mono: &[f32]) -> u64 {
        mono.iter().fold(0xcbf29ce484222325, |mut hash, sample| {
            let pcm = (sample * 32768.0) as i16;
            for byte in pcm.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
            hash
        })
    }

    #[test]
    fn native_mono_pcm_matches_deadsync() {
        let decoded = decode_ogg_symphonia(&fixture_path("mono.ogg")).expect("decode mono OGG");
        assert_eq!(decoded.sample_rate_hz, 44_100);
        assert_eq!(decoded.mono.len(), 11_025);
        assert_eq!(pcm_hash(&decoded.mono), 0x553b7e846404c5f8);
    }

    #[test]
    fn native_stereo_pcm_matches_deadsync_with_channel_max() {
        let decoded = decode_ogg_symphonia(&fixture_path("stereo.ogg")).expect("decode stereo OGG");
        assert_eq!(decoded.sample_rate_hz, 44_100);
        assert_eq!(decoded.mono.len(), 55_125);
        assert_eq!(pcm_hash(&decoded.mono), 0xc6dfbe8a35eb7c80);
    }

    #[test]
    fn missing_legacy_granules_match_deadsync_and_retain_audio() {
        let path = fixture_path("stereo.ogg");
        assert!(!has_legacy_missing_granules_in_prefix(&path).expect("inspect fixture"));
        let expected = decode_ogg_symphonia(&path).expect("decode original OGG");
        let mut bytes = fs::read(&path).expect("read OGG fixture");
        let mut offset = 0;
        let mut modified = false;
        while offset + OGG_PAGE_HEADER_LEN <= bytes.len() {
            assert_eq!(&bytes[offset..offset + 4], b"OggS");
            let segment_start = offset + OGG_PAGE_HEADER_LEN;
            let segment_end = segment_start + usize::from(bytes[offset + 26]);
            let body_len: usize = bytes[segment_start..segment_end]
                .iter()
                .map(|&length| usize::from(length))
                .sum();
            let page_end = segment_end + body_len;
            let sequence = u32::from_le_bytes(bytes[offset + 18..offset + 22].try_into().unwrap());
            if sequence == 2 {
                assert_eq!(bytes[offset + 5] & 4, 0, "modify a non-final audio page");
                bytes[offset + 6..offset + 14].fill(u8::MAX);
                bytes[offset + 22..offset + 26].fill(0);
                let mut crc = Crc32::new(0);
                crc.process_buf_bytes(&bytes[offset..page_end]);
                bytes[offset + 22..offset + 26].copy_from_slice(&crc.crc().to_le_bytes());
                modified = true;
            }
            offset = page_end;
        }
        assert!(modified, "fixture has an audio page");
        let legacy = TempOgg::new(&bytes);
        assert!(has_legacy_missing_granules_in_prefix(&legacy.0).expect("inspect legacy OGG"));
        let actual = decode_ogg_symphonia(&legacy.0).expect("decode legacy OGG");
        assert_eq!(actual.sample_rate_hz, expected.sample_rate_hz);
        // With missing granules, deadsync retains the final block's padding.
        assert_eq!(actual.mono.len(), 55_872);
        assert_eq!(pcm_hash(&actual.mono), 0xd46d83a5b1a822ba);
        assert_eq!(
            pcm_hash(&actual.mono[..expected.mono.len()]),
            pcm_hash(&expected.mono)
        );
    }

    #[test]
    fn malformed_ogg_returns_error_with_path() {
        let invalid = TempOgg::new(b"not an OGG stream");
        let error = decode_ogg_symphonia(&invalid.0).expect_err("reject invalid OGG");
        assert!(error.contains(&invalid.0.display().to_string()));
    }

    #[test]
    fn ogg_without_audio_returns_error() {
        let bytes = fs::read(fixture_path("mono.ogg")).expect("read OGG fixture");
        let audio_page = bytes
            .windows(4)
            .enumerate()
            .filter(|(_, window)| *window == b"OggS")
            .nth(2)
            .expect("fixture has an audio page")
            .0;
        let empty = TempOgg::new(&bytes[..audio_page]);
        assert!(decode_ogg_symphonia(&empty.0).is_err());
    }

    #[test]
    fn stereo_collapse_uses_channel_max() {
        let mut out = Vec::new();
        append_python_mono_like(&[100, 200, -3200, -6400], 2, &mut out);
        assert_eq!(out.len(), 2);
        assert!((out[0] - (200.0 / 32768.0)).abs() < 1e-7);
        assert!((out[1] - (-3200.0 / 32768.0)).abs() < 1e-7);
    }

    #[test]
    fn mono_passthrough_is_normalized() {
        let mut out = Vec::new();
        append_python_mono_like(&[32767, 0, -32768], 1, &mut out);
        assert_eq!(out.len(), 3);
        assert!((out[0] - (32767.0 / 32768.0)).abs() < 1e-7);
        assert_eq!(out[1], 0.0);
        assert_eq!(out[2], -1.0);
    }

    #[test]
    fn multichannel_passthrough_stays_interleaved() {
        let mut out = Vec::new();
        append_python_mono_like(&[1, 10, 100, 2, 20, 200], 3, &mut out);
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
