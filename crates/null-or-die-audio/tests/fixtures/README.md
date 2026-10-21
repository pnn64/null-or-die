# Audio decoder fixtures

These synthetic signals were generated with FFmpeg's `libvorbis` encoder:

```sh
ffmpeg -f lavfi -i 'sine=frequency=440:sample_rate=44100:duration=0.25' -c:a libvorbis -q:a 2 -map_metadata -1 -fflags +bitexact -flags:a +bitexact mono.ogg
ffmpeg -f lavfi -i 'aevalsrc=0.25*sin(2*PI*440*t)|-0.5*sin(2*PI*660*t):s=44100:d=1.25' -c:a libvorbis -q:a 2 -map_metadata -1 -fflags +bitexact -flags:a +bitexact stereo.ogg
```

The expected PCM hashes in the tests were checked sample for sample against
`deadsync/crates/deadlib-audio-decode/src/ogg_vorbis.rs`, using Symphonia 0.6.1,
converting to interleaved PCM16 and then applying null-or-die's signed stereo
channel maximum. The tests use the committed files and do not require FFmpeg
or a deadsync checkout.

The legacy-granule regression test rewrites the first non-final audio page's
granule position to `u64::MAX` and recalculates its OGG checksum. Decoded PCM
must retain all original audio and match deadsync's legacy-file output,
including the final block's padding when granules are missing.
