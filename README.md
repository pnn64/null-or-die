# null-or-die

Terminal-first Rust reimplementation scaffold for `nine-or-null` parity work.

## Why This Project Exists

`nod` is a rewrite of [`nine-or-null`](https://github.com/telperion/nine-or-null) by [@telperion](https://github.com/telperion).

Why:

1. Performance advantages from implementing the sync-analysis pipeline in a lower-level language (Rust).
2. Acting as a submodule/integration target for [`deadsync`](https://github.com/pnn64/deadsync), the Rust rewrite of ITGmania/StepMania.

## Current status

- `nod analyze <path> [--plot]`: scans simfiles, parses chart metadata through `rssp`, decodes OGG audio, and computes native bias metrics per chart. With `--plot`, also writes nine-or-null-style PNGs (`bias-freqdomain-*`, `bias-beatdigest-*`, `bias-postkernel-*`) into the report directory.
- `nod parity <path> --baseline <dir>`: validates MD5-sharded fixtures and checks native bias outputs against baseline chart rows (including split `#MUSIC` rows).
- `nod harness <path> --baseline <dir>`: runs Python `nine-or-null` reference analysis and writes canonical `json.zst` fixtures.
- `nod bench <simfile>`: runs repeated analyze-style passes on one simfile and reports phase timings (read, parse, decode, bias, total).
- `nod plot <input.json> <out.png>`: draws bias markers from JSON (`bias_ms`, `bias_result`, or `bias`).

`analyze` and `parity` now share the native bias math path.

For `harness`, `--source-root` should point to the Python package root containing `nine_or_null/` (for example `nine-or-null-0.8.0/nine-or-null`). If omitted, `nod` auto-detects that sibling path from the current working directory.

## Baseline layout

MD5-sharded baseline lookup matches the existing `rssp` corpus style:

`<baseline>/<md5[0..2]>/<md5>.json` or `<baseline>/<md5[0..2]>/<md5>.json.zst`

MD5 is computed from raw simfile bytes.

Baseline chart rows include a `music` field (chart `#MUSIC` if present, else simfile `#MUSIC`) so split-audio parity can target the correct OGG per chart.

## Examples

```bash
cargo run -- analyze /path/to/Songs --output /tmp/nod-scan.json
cargo run -- analyze /path/to/song.sm --plot --report-path /tmp/nod-plots
# legacy-style invocation is also supported:
cargo run -- --analyze /path/to/song.sm --plot --report-path /tmp/nod-plots
cargo run -- parity /path/to/Songs --baseline /path/to/baseline --fail-on-missing --fail-on-mismatch
cargo run -- harness /path/to/Songs --baseline /path/to/baseline --source-root /path/to/nine-or-null-0.8.0/nine-or-null
cargo run --release -- bench "/path/to/PEMDMonium.sm" --warmup 3 --iterations 20
cargo run -- plot /tmp/nod-scan.json /tmp/bias.png --span-ms 20
```

## Library API (for deadsync integration)

`nod` now exposes a direct Rust API for in-engine sync tooling:

- `nod::api::inspect_simfile(path)` -> chart metadata for selection/UI
- `nod::api::analyze_chart(path, chart_index, &cfg)` -> full bias estimate + full graph matrices
- `nod::api::analyze_chart_stream(path, chart_index, &cfg, stream_cfg, on_event)` -> incremental events while processing (`Init`, `Beat`, `Convolution`, `Done`) plus final result

For streamed rendering layout, use `stream_cfg.orientation` (`Vertical` or `Horizontal`) as display intent in your actor/UI layer.
