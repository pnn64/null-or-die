use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use null_or_die_core::{
    BiasCfg, BiasKernel, BiasRuntime, KernelTarget, estimate_bias_with_beat_fn_reuse,
};

struct CountAlloc;

static TRACK: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
static REALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static GLOBAL: CountAlloc = CountAlloc;

// SAFETY: every operation delegates to the system allocator with the exact
// pointer and layout supplied by the caller. The atomics only observe calls.
unsafe impl GlobalAlloc for CountAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegated with the caller-provided layout.
        let ptr = unsafe { System.alloc(layout) };
        if TRACK.load(Ordering::Relaxed) && !ptr.is_null() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegated with the caller-provided layout.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if TRACK.load(Ordering::Relaxed) && !ptr.is_null() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACK.load(Ordering::Relaxed) {
            DEALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: delegated with the caller-provided pointer and layout.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: delegated with the caller-provided pointer, layout, and size.
        let next = unsafe { System.realloc(ptr, layout, new_size) };
        if TRACK.load(Ordering::Relaxed) && !next.is_null() {
            REALLOCS.fetch_add(1, Ordering::Relaxed);
            REALLOC_BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        next
    }
}

fn main() {
    let iterations = env_usize("NOD_BENCH_ITERS", 7).max(1);
    let seconds = env_usize("NOD_BENCH_SECONDS", 30).max(4);
    let sample_rate_hz = 44_100u32;
    let audio = synth_audio(sample_rate_hz, seconds);
    let cfg = BiasCfg {
        fingerprint_ms: 50.0,
        window_ms: 10.0,
        step_ms: 0.2,
        magic_offset_ms: 0.0,
        kernel_target: KernelTarget::Digest,
        kernel_type: BiasKernel::Rising,
        _full_spectrogram: false,
    };
    let mut runtime = BiasRuntime::default();
    run_estimate(&audio, sample_rate_hz, &cfg, &mut runtime);

    let mut nanos = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let estimate = run_estimate(&audio, sample_rate_hz, &cfg, &mut runtime);
        nanos.push(start.elapsed().as_nanos());
        black_box(estimate);
    }

    nanos.sort_unstable();
    let total_ns = nanos.iter().sum::<u128>();
    let avg_ns = total_ns as f64 / iterations as f64;
    let median_ns = nanos[nanos.len() / 2] as f64;

    reset_counts();
    TRACK.store(true, Ordering::Relaxed);
    for _ in 0..iterations {
        black_box(run_estimate(&audio, sample_rate_hz, &cfg, &mut runtime));
    }
    TRACK.store(false, Ordering::Relaxed);

    println!("benchmark=bias_estimate");
    println!("audio_seconds={seconds}");
    println!("iterations={iterations}");
    println!("avg_ms={:.3}", avg_ns / 1e6);
    println!("median_ms={:.3}", median_ns / 1e6);
    println!("throughput_per_sec={:.3}", 1e9 / avg_ns);
    print_count("alloc_calls_per_iter", &ALLOCS, iterations);
    print_count("realloc_calls_per_iter", &REALLOCS, iterations);
    print_count("dealloc_calls_per_iter", &DEALLOCS, iterations);
    print_count("allocated_bytes_per_iter", &ALLOC_BYTES, iterations);
    print_count("reallocated_bytes_per_iter", &REALLOC_BYTES, iterations);
}

fn run_estimate(
    audio: &[f32],
    sample_rate_hz: u32,
    cfg: &BiasCfg,
    runtime: &mut BiasRuntime,
) -> null_or_die_core::BiasEstimate {
    estimate_bias_with_beat_fn_reuse(audio, sample_rate_hz, cfg, runtime, |beat| {
        beat as f64 * 0.5
    })
    .expect("synthetic benchmark estimate should succeed")
}

fn synth_audio(sample_rate_hz: u32, seconds: usize) -> Vec<f32> {
    let len = sample_rate_hz as usize * seconds;
    let mut audio = Vec::with_capacity(len);
    for i in 0..len {
        let t = i as f64 / f64::from(sample_rate_hz);
        let carrier = (t * 440.0 * std::f64::consts::TAU).sin();
        let pulse = (t * 2.0 * std::f64::consts::TAU).sin().max(0.0);
        audio.push((carrier * pulse * 0.8) as f32);
    }
    audio
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(default)
}

fn reset_counts() {
    ALLOCS.store(0, Ordering::Relaxed);
    REALLOCS.store(0, Ordering::Relaxed);
    DEALLOCS.store(0, Ordering::Relaxed);
    ALLOC_BYTES.store(0, Ordering::Relaxed);
    REALLOC_BYTES.store(0, Ordering::Relaxed);
}

fn print_count(name: &str, value: &AtomicUsize, iterations: usize) {
    println!(
        "{name}={:.3}",
        value.load(Ordering::Relaxed) as f64 / iterations as f64
    );
}
