//! Preset sweep with system-resource sampling, plus a long-session soak.
//!
//!   ./dev-build.sh example stream_bench -- <16k-mono.wav> [gpu|cpu] [sweep|long|silence]
//!
//! Sweep times each valid (C,R) tuple at the model's fixed left context and
//! reports RTF, CPU% of one core, GPU busy/power and peak RSS. `long` streams
//! ten minutes to check stability. `silence` measures the cost of feeding
//! non-speech, which decides whether VAD-gated feeding is worth it.

use std::fs;
use std::time::{Duration, Instant};

use arxell_speak_lib::engine::{DEFAULT_MODEL_SHA256, DEFAULT_MODEL_SIZE};
use transcribe_cpp::{
    Backend, CommitPolicy, Model, ModelOptions, ParakeetBufferedStreamOptions, RunOptions,
    StreamExtension, StreamOptions, init_backends_default,
};

const CLOCK_TICKS: f64 = 100.0;

#[derive(Clone, Copy)]
struct Usage {
    cpu_ticks: u64,
    gpu_busy: u32,
    gpu_watts: f64,
    rss_kb: u64,
}

fn read_usage() -> Usage {
    let stat = fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let fields: Vec<&str> = stat.split_whitespace().collect();
    let cpu_ticks = fields
        .get(13)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
        + fields
            .get(14)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
    let mut gpu_busy = 0;
    let mut gpu_watts = 0.0;
    if let Ok(cards) = fs::read_dir("/sys/class/drm") {
        for card in cards.flatten() {
            let name = card.file_name();
            if !name.to_string_lossy().starts_with("card") {
                continue;
            }
            let base = card.path().join("device");
            if let Ok(text) = fs::read_to_string(base.join("gpu_busy_percent")) {
                gpu_busy = text.trim().parse().unwrap_or(0);
            }
            if let Ok(hwmons) = fs::read_dir(base.join("hwmon")) {
                for hw in hwmons.flatten() {
                    if let Ok(text) = fs::read_to_string(hw.path().join("power1_average")) {
                        gpu_watts = text.trim().parse::<f64>().unwrap_or(0.0) / 1_000_000.0;
                    }
                }
            }
        }
    }
    let rss_kb = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0);
    Usage {
        cpu_ticks,
        gpu_busy,
        gpu_watts,
        rss_kb,
    }
}

struct Meter {
    start: Usage,
    started: Instant,
    gpu_samples: u64,
    gpu_busy_sum: u64,
    gpu_watts_sum: f64,
    rss_max: u64,
    last_sample: Instant,
}

impl Meter {
    fn new() -> Self {
        Self {
            start: read_usage(),
            started: Instant::now(),
            gpu_samples: 0,
            gpu_busy_sum: 0,
            gpu_watts_sum: 0.0,
            rss_max: 0,
            last_sample: Instant::now(),
        }
    }
    fn maybe_sample(&mut self) {
        if self.last_sample.elapsed() < Duration::from_millis(1000) {
            return;
        }
        let usage = read_usage();
        self.gpu_samples += 1;
        self.gpu_busy_sum += usage.gpu_busy as u64;
        self.gpu_watts_sum += usage.gpu_watts;
        self.rss_max = self.rss_max.max(usage.rss_kb);
        self.last_sample = Instant::now();
    }
    fn finish(&mut self) -> String {
        self.maybe_sample();
        let end = read_usage();
        let wall = self.started.elapsed().as_secs_f64();
        let cpu_pct =
            (end.cpu_ticks.saturating_sub(self.start.cpu_ticks)) as f64 / CLOCK_TICKS / wall
                * 100.0;
        let busy = self
            .gpu_busy_sum
            .checked_div(self.gpu_samples)
            .unwrap_or(end.gpu_busy as u64);
        let watts = if self.gpu_samples == 0 {
            end.gpu_watts
        } else {
            self.gpu_watts_sum / self.gpu_samples as f64
        };
        format!(
            "cpu={cpu_pct:.0}%core gpu={busy}% gpu_power={watts:.1}W rss={}MB",
            self.rss_max.max(end.rss_kb) / 1024
        )
    }
}

fn load_pcm(path: &str, seconds: usize) -> Result<Vec<f32>, String> {
    let mut wav = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = wav.spec();
    if spec.channels != 1 || spec.bits_per_sample != 16 || spec.sample_rate != 16_000 {
        return Err("expected mono 16 kHz PCM16 WAV".into());
    }
    let base: Vec<f32> = wav
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    if seconds == 0 {
        return Ok(base);
    }
    let silence = vec![0.0f32; 4_800];
    let mut pcm = Vec::with_capacity(seconds * 16_000);
    while pcm.len() < seconds * 16_000 {
        pcm.extend_from_slice(&base);
        pcm.extend_from_slice(&silence);
    }
    Ok(pcm)
}

fn stream(model: &Model, pcm: &[f32], c: i32, r: i32) -> Result<(f64, String, Duration), String> {
    let options = StreamOptions {
        commit_policy: CommitPolicy::Auto,
        family: Some(StreamExtension::ParakeetBuffered(
            ParakeetBufferedStreamOptions {
                left_ms: Some(5_600),
                chunk_ms: Some(c * 80),
                right_ms: Some(r * 80),
            },
        )),
        ..Default::default()
    };
    let mut session = model.session().map_err(|e| e.to_string())?;
    let mut stream = session
        .stream(&RunOptions::default(), &options)
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let mut max_feed = Duration::ZERO;
    for frame in pcm.chunks(256) {
        let t = Instant::now();
        stream.feed(frame).map_err(|e| e.to_string())?;
        max_feed = max_feed.max(t.elapsed());
    }
    stream.finalize().map_err(|e| e.to_string())?;
    Ok((
        started.elapsed().as_secs_f64(),
        stream.text().full,
        max_feed,
    ))
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let wav = args
        .get(1)
        .ok_or("usage: stream_bench <wav> [gpu|cpu] [sweep|long|silence]")?;
    let backend = if args.get(2).is_some_and(|s| s == "cpu") {
        Backend::Cpu
    } else {
        Backend::Vulkan
    };
    let mode = args.get(3).map(String::as_str).unwrap_or("sweep");

    let model_path = std::env::var("ARXELL_MODEL").map_err(|_| "set ARXELL_MODEL")?;
    let model_path = std::path::Path::new(&model_path);
    arxell_speak_lib::engine::verify_model(
        model_path,
        DEFAULT_MODEL_SHA256,
        Some(DEFAULT_MODEL_SIZE),
    )?;
    init_backends_default().map_err(|e| e.to_string())?;
    let model = Model::load_with(
        model_path,
        &ModelOptions {
            backend,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    println!("backend={:?}", model.backend());

    if mode == "long" {
        let pcm = load_pcm(wav, 600)?;
        let audio_s = pcm.len() as f64 / 16_000.0;
        let mut meter = Meter::new();
        let mut last_report = Instant::now();
        let options = StreamOptions {
            commit_policy: CommitPolicy::Auto,
            family: Some(StreamExtension::ParakeetBuffered(
                ParakeetBufferedStreamOptions {
                    left_ms: Some(5_600),
                    chunk_ms: Some(13 * 80),
                    right_ms: Some(2 * 80),
                },
            )),
            ..Default::default()
        };
        let mut session = model.session().map_err(|e| e.to_string())?;
        let mut stream = session
            .stream(&RunOptions::default(), &options)
            .map_err(|e| e.to_string())?;
        let started = Instant::now();
        for (index, frame) in pcm.chunks(256).enumerate() {
            stream.feed(frame).map_err(|e| e.to_string())?;
            meter.maybe_sample();
            if last_report.elapsed() >= Duration::from_secs(30) {
                println!(
                    "  t={:.0}s audio={:.0}s {}",
                    started.elapsed().as_secs_f64(),
                    index as f64 * 256.0 / 16_000.0,
                    meter.finish()
                );
                last_report = Instant::now();
            }
        }
        stream.finalize().map_err(|e| e.to_string())?;
        let wall = started.elapsed().as_secs_f64();
        let text = stream.text().full;
        println!(
            "long audio={audio_s:.0}s wall={wall:.1}s rtf={:.3} {} words={} country_count={}",
            wall / audio_s,
            meter.finish(),
            text.split_whitespace().count(),
            text.matches("country").count()
        );
        return Ok(());
    }

    let (pcm, label) = if mode == "silence" {
        (vec![0.0f32; 60 * 16_000], "silence".to_string())
    } else {
        let pcm = load_pcm(wav, 60)?;
        let audio_s = pcm.len() as f64 / 16_000.0;
        (pcm, format!("{audio_s:.0}s"))
    };
    let audio_s = pcm.len() as f64 / 16_000.0;

    // Offline reference for quality comparison.
    let mut meter = Meter::new();
    let reference = model
        .session()
        .map_err(|e| e.to_string())?
        .run(&pcm, &RunOptions::default())
        .map_err(|e| e.to_string())?
        .text;
    println!(
        "offline rtf={:.3} {}",
        meter.started.elapsed().as_secs_f64() / audio_s,
        meter.finish()
    );

    let configs: &[(i32, i32)] = &[
        (2, 2),
        (7, 2),
        (7, 4),
        (7, 7),
        (13, 0),
        (13, 2),
        (13, 7),
        (13, 13),
    ];
    let ref_words: Vec<&str> = reference.split_whitespace().collect();
    for &(c, r) in configs {
        let mut meter = Meter::new();
        match stream(&model, &pcm, c, r) {
            Ok((wall, text, max_feed)) => {
                let out: Vec<&str> = text.split_whitespace().collect();
                let matches = out
                    .iter()
                    .zip(ref_words.iter())
                    .filter(|(a, b)| a == b)
                    .count();
                println!(
                    "(70,{c},{r}) lookahead={}ms rtf={:.3} max_feed={}ms match={}/{} {}",
                    (c + r) * 80,
                    wall / audio_s,
                    max_feed.as_millis(),
                    matches,
                    ref_words.len(),
                    meter.finish()
                );
            }
            Err(error) => println!("(70,{c},{r}) rejected: {error}"),
        }
    }
    println!("input={label}");
    Ok(())
}
