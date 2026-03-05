use std::fs;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use png::{BitDepth, ColorType, Encoder};
use serde_json::Value;

use crate::bias::{BiasPlotData, GraphOrientation};
use crate::cli::PlotCmd;
use crate::model::{KernelTarget, PlotReport};

pub fn run(args: &PlotCmd) -> Result<PlotReport, String> {
    if args.width == 0 || args.height == 0 {
        return Err("width and height must be > 0".to_string());
    }
    let text = fs::read_to_string(&args.input_json)
        .map_err(|e| format!("read {} failed: {e}", args.input_json.display()))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("parse {} failed: {e}", args.input_json.display()))?;
    let mut biases = Vec::new();
    collect_biases(&value, &mut biases);
    if biases.is_empty() {
        return Err("no bias values found in JSON (bias_ms / bias_result / bias)".to_string());
    }
    write_bias_plot(
        &args.output_png,
        args.width,
        args.height,
        args.span_ms,
        &biases,
    )?;
    Ok(PlotReport {
        tool: "nod".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        input_json: args.input_json.display().to_string(),
        output_png: args.output_png.display().to_string(),
        width: args.width,
        height: args.height,
        span_ms: args.span_ms,
        bias_count: biases.len(),
    })
}

fn collect_biases(value: &Value, out: &mut Vec<f64>) {
    match value {
        Value::Object(map) => {
            for key in ["bias_ms", "bias_result", "bias"] {
                if let Some(v) = map.get(key).and_then(parse_bias) {
                    out.push(v);
                }
            }
            for v in map.values() {
                collect_biases(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_biases(v, out);
            }
        }
        _ => {}
    }
}

fn parse_bias(value: &Value) -> Option<f64> {
    if let Some(v) = value.as_f64() {
        Some(v)
    } else {
        value.as_str().and_then(|s| s.parse::<f64>().ok())
    }
}

fn write_bias_plot(
    path: &Path,
    width: u32,
    height: u32,
    span_ms: f64,
    biases: &[f64],
) -> Result<(), String> {
    let mut image = vec![255u8; (width as usize) * (height as usize) * 4];
    let center_x = width / 2;
    draw_vline(&mut image, width, height, center_x, [96, 96, 96, 255]);
    for bias in biases {
        let x = bias_to_x(*bias, span_ms, width);
        draw_vline(&mut image, width, height, x, [220, 40, 40, 255]);
    }
    write_rgba_png(path, width, height, &image)
}

fn bias_to_x(bias_ms: f64, span_ms: f64, width: u32) -> u32 {
    let span = if span_ms.abs() < f64::EPSILON {
        50.0
    } else {
        span_ms
    };
    let normalized = ((bias_ms + span) / (span * 2.0)).clamp(0.0, 1.0);
    (normalized * f64::from(width.saturating_sub(1))).round() as u32
}

fn draw_vline(image: &mut [u8], width: u32, height: u32, x: u32, rgba: [u8; 4]) {
    for y in 0..height {
        let idx = ((y * width + x) * 4) as usize;
        image[idx] = rgba[0];
        image[idx + 1] = rgba[1];
        image[idx + 2] = rgba[2];
        image[idx + 3] = rgba[3];
    }
}

pub fn write_nine_or_null_plots(
    report_dir: &Path,
    stem: &str,
    plot: &BiasPlotData,
) -> Result<(), String> {
    write_nine_or_null_plots_oriented(report_dir, stem, plot, GraphOrientation::Vertical)
}

pub fn write_nine_or_null_plots_oriented(
    report_dir: &Path,
    stem: &str,
    plot: &BiasPlotData,
    orientation: GraphOrientation,
) -> Result<(), String> {
    if stem.trim().is_empty() {
        return Err("plot stem is empty".to_string());
    }
    if plot.cols == 0 {
        return Err("plot has zero columns".to_string());
    }
    fs::create_dir_all(report_dir)
        .map_err(|e| format!("create plot dir {} failed: {e}", report_dir.display()))?;
    let dims = (600_u32, 600_u32);
    let freq_path = report_dir.join(format!("bias-freqdomain-{stem}.png"));
    write_heat_plot(
        &freq_path,
        plot.freq_domain.as_slice(),
        plot.freq_rows,
        plot.cols,
        plot.times_ms.as_slice(),
        plot.freqs_khz.as_slice(),
        plot.convolution.as_slice(),
        plot.edge_discard,
        plot.bias_ms,
        None,
        dims,
        orientation,
    )?;
    let digest_path = report_dir.join(format!("bias-beatdigest-{stem}.png"));
    write_heat_plot(
        &digest_path,
        plot.beat_digest.as_slice(),
        plot.digest_rows,
        plot.cols,
        plot.times_ms.as_slice(),
        plot.beat_indices.as_slice(),
        plot.convolution.as_slice(),
        plot.edge_discard,
        plot.bias_ms,
        Some((10.0, 90.0)),
        dims,
        orientation,
    )?;
    let post_path = report_dir.join(format!("bias-postkernel-{stem}.png"));
    let post_y = if plot.post_target == KernelTarget::Accumulator {
        plot.freqs_khz.as_slice()
    } else {
        plot.beat_indices.as_slice()
    };
    write_heat_plot(
        &post_path,
        plot.post_kernel.as_slice(),
        plot.post_rows,
        plot.cols,
        plot.times_ms.as_slice(),
        post_y,
        plot.convolution.as_slice(),
        plot.edge_discard,
        plot.bias_ms,
        Some((3.0, 97.0)),
        dims,
        orientation,
    )
}

#[allow(clippy::too_many_arguments)]
fn write_heat_plot(
    out_path: &Path,
    matrix: &[f64],
    rows: usize,
    cols: usize,
    times_ms: &[f64],
    y_axis: &[f64],
    convolution: &[f64],
    edge_discard: usize,
    bias_ms: f64,
    clim_pct: Option<(f64, f64)>,
    dims: (u32, u32),
    orientation: GraphOrientation,
) -> Result<(), String> {
    if rows == 0 || cols == 0 {
        return Err(format!(
            "plot matrix has invalid shape rows={rows} cols={cols}"
        ));
    }
    if matrix.len() != rows.saturating_mul(cols) {
        return Err(format!(
            "plot matrix shape mismatch: len={} rows={} cols={}",
            matrix.len(),
            rows,
            cols
        ));
    }
    if times_ms.len() != cols || y_axis.len() != rows || convolution.len() != cols {
        return Err(format!(
            "plot axis mismatch: times={} y={} conv={} rows={} cols={}",
            times_ms.len(),
            y_axis.len(),
            convolution.len(),
            rows,
            cols
        ));
    }
    let (width, height) = dims;
    let (z_lo, z_hi) = value_range(matrix, clim_pct);
    let (y_lo, y_hi) = axis_minmax(y_axis);
    let mut image = vec![0u8; (width as usize) * (height as usize) * 4];
    for py in 0..height {
        for px in 0..width {
            let (row, col) = if orientation == GraphOrientation::Vertical {
                (
                    (((height - 1 - py) as usize) * rows / height as usize).min(rows - 1),
                    ((px as usize) * cols / width as usize).min(cols - 1),
                )
            } else {
                (
                    ((px as usize) * rows / width as usize).min(rows - 1),
                    (((height - 1 - py) as usize) * cols / height as usize).min(cols - 1),
                )
            };
            let val = matrix[row * cols + col];
            let t = norm01(val, z_lo, z_hi);
            let rgb = viridis(t);
            let idx = ((py * width + px) * 4) as usize;
            image[idx] = rgb[0];
            image[idx + 1] = rgb[1];
            image[idx + 2] = rgb[2];
            image[idx + 3] = 255;
        }
    }
    if orientation == GraphOrientation::Vertical {
        let red_x = time_to_px(bias_ms, times_ms, width);
        draw_vline(&mut image, width, height, red_x, [220, 20, 20, 255]);
    } else {
        let red_y = y_to_px(
            bias_ms,
            times_ms[0],
            *times_ms.last().unwrap_or(&times_ms[0]),
            height,
        );
        draw_hline(&mut image, width, height, red_y, [220, 20, 20, 255]);
    }
    draw_conv_line(
        &mut image,
        width,
        height,
        times_ms,
        y_lo,
        y_hi,
        convolution,
        edge_discard,
        orientation,
    );
    write_rgba_png(out_path, width, height, &image)
}

fn value_range(values: &[f64], pct: Option<(f64, f64)>) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 1.0);
    }
    if let Some((lo, hi)) = pct {
        let v_lo = percentile(values, lo);
        let v_hi = percentile(values, hi);
        if v_hi > v_lo {
            return (v_lo, v_hi);
        }
    }
    let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if hi > lo {
        (lo, hi)
    } else {
        (lo - 1.0, hi + 1.0)
    }
}

fn axis_minmax(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 1.0);
    }
    let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if hi > lo {
        (lo, hi)
    } else {
        (lo - 1.0, hi + 1.0)
    }
}

fn draw_conv_line(
    image: &mut [u8],
    width: u32,
    height: u32,
    times_ms: &[f64],
    y_min: f64,
    y_max: f64,
    conv: &[f64],
    edge_discard: usize,
    orientation: GraphOrientation,
) {
    if conv.len() < 3 {
        return;
    }
    let edge = edge_discard.min(conv.len() / 2);
    let core = &conv[edge..conv.len() - edge];
    let c_lo = core.iter().copied().fold(f64::INFINITY, f64::min);
    let c_hi = core.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let y_from = y_min * 0.9 + y_max * 0.1;
    let y_to = y_min * 0.1 + y_max * 0.9;
    let mut prev: Option<(i32, i32)> = None;
    for i in 0..conv.len() {
        let y_val = lerp(y_from, y_to, norm01(conv[i], c_lo, c_hi));
        let (x, y) = if orientation == GraphOrientation::Vertical {
            (
                time_to_px(times_ms[i], times_ms, width) as i32,
                y_to_px(y_val, y_min, y_max, height) as i32,
            )
        } else {
            (
                x_to_px(y_val, y_min, y_max, width) as i32,
                y_to_px(
                    times_ms[i],
                    times_ms[0],
                    *times_ms.last().unwrap_or(&times_ms[0]),
                    height,
                ) as i32,
            )
        };
        if let Some((px, py)) = prev {
            draw_line(image, width, height, px, py, x, y, [255, 255, 255, 255]);
        }
        prev = Some((x, y));
    }
}

fn draw_line(
    image: &mut [u8],
    width: u32,
    height: u32,
    mut x0: i32,
    mut y0: i32,
    x1: i32,
    y1: i32,
    rgba: [u8; 4],
) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        put_px(image, width, height, x0, y0, rgba);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = err * 2;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

fn put_px(image: &mut [u8], width: u32, height: u32, x: i32, y: i32, rgba: [u8; 4]) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let idx = (((y as u32) * width + (x as u32)) * 4) as usize;
    image[idx] = rgba[0];
    image[idx + 1] = rgba[1];
    image[idx + 2] = rgba[2];
    image[idx + 3] = rgba[3];
}

fn draw_hline(image: &mut [u8], width: u32, height: u32, y: u32, rgba: [u8; 4]) {
    if y >= height {
        return;
    }
    for x in 0..width {
        let idx = ((y * width + x) * 4) as usize;
        image[idx] = rgba[0];
        image[idx + 1] = rgba[1];
        image[idx + 2] = rgba[2];
        image[idx + 3] = rgba[3];
    }
}

fn time_to_px(time_ms: f64, times_ms: &[f64], width: u32) -> u32 {
    if times_ms.is_empty() || width <= 1 {
        return 0;
    }
    let lo = times_ms[0];
    let hi = *times_ms.last().unwrap_or(&lo);
    let t = norm01(time_ms, lo, hi);
    (t * f64::from(width - 1)).round() as u32
}

fn y_to_px(y_val: f64, y_min: f64, y_max: f64, height: u32) -> u32 {
    if height <= 1 {
        return 0;
    }
    let t = norm01(y_val, y_min, y_max);
    ((1.0 - t) * f64::from(height - 1)).round() as u32
}

fn x_to_px(x_val: f64, x_min: f64, x_max: f64, width: u32) -> u32 {
    if width <= 1 {
        return 0;
    }
    let t = norm01(x_val, x_min, x_max);
    (t * f64::from(width - 1)).round() as u32
}

fn norm01(v: f64, lo: f64, hi: f64) -> f64 {
    let span = hi - lo;
    if !span.is_finite() || span.abs() < f64::EPSILON {
        0.5
    } else {
        ((v - lo) / span).clamp(0.0, 1.0)
    }
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a * (1.0 - t) + b * t
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        lerp(sorted[lo], sorted[hi], rank - lo as f64)
    }
}

fn viridis(t: f64) -> [u8; 3] {
    const STOPS: [[u8; 3]; 5] = [
        [68, 1, 84],
        [59, 82, 139],
        [33, 145, 140],
        [94, 201, 98],
        [253, 231, 37],
    ];
    let x = t.clamp(0.0, 1.0) * 4.0;
    let i = x.floor() as usize;
    if i >= 4 {
        return STOPS[4];
    }
    let frac = x - i as f64;
    let a = STOPS[i];
    let b = STOPS[i + 1];
    [
        lerp(a[0] as f64, b[0] as f64, frac).round() as u8,
        lerp(a[1] as f64, b[1] as f64, frac).round() as u8,
        lerp(a[2] as f64, b[2] as f64, frac).round() as u8,
    ]
}

fn write_rgba_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let file = File::create(path).map_err(|e| format!("create {} failed: {e}", path.display()))?;
    let writer = BufWriter::new(file);
    let mut encoder = Encoder::new(writer, width, height);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    let mut png_writer = encoder
        .write_header()
        .map_err(|e| format!("png header {} failed: {e}", path.display()))?;
    png_writer
        .write_image_data(rgba)
        .map_err(|e| format!("png write {} failed: {e}", path.display()))
}
