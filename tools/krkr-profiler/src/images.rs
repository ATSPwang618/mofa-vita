use clap::Args;
use krkr_assets::{Vfs, name::units};
use krkr_protocol::{budget::Budget, profile};
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Instant};

#[derive(Args)]
pub struct Options {
    #[arg(long)]
    game: PathBuf,
    #[arg(long)]
    out: PathBuf,
    /// BC storage names, including archive entries such as image.xp3>name.kbct.
    #[arg(long, required = true)]
    image: Vec<String>,
    #[arg(long, default_value = "5")]
    samples: std::num::NonZeroU8,
    #[arg(long, default_value = "32")]
    budget_mib: std::num::NonZeroU16,
}

pub fn run(options: Options) -> Result<(), String> {
    std::fs::create_dir_all(&options.out).map_err(|e| e.to_string())?;
    let capture = profile::FileCapture::start(&options.out.join("events.jsonl"), 4096)?;
    let mut vfs = Vfs::new(&options.game, Default::default()).map_err(|e| e.to_string())?;
    let bytes = usize::from(options.budget_mib.get())
        .checked_mul(1024 * 1024)
        .ok_or("image workspace budget overflow")?;
    let budget = Budget::new(bytes);
    budget.set_profile_name("image.workspace_bytes");
    let cancelled = AtomicBool::new(false);
    let mut rows = Vec::new();
    for name in &options.image {
        let mut times = Vec::new();
        let mut expected = None;
        for sample in 0..=options.samples.get() {
            let plan = vfs.plan(&units(name)).map_err(|e| e.to_string())?;
            let encoded_bytes = plan.bytes;
            let request = krkr_image::Request::from_plans(
                plan,
                None,
                None,
                0x02ffffff,
                None,
                false,
                budget.clone(),
            );
            profile::marker("image.sample", || format!("name={name} sample={sample}"));
            let start = Instant::now();
            let prepared = request.probe(&cancelled).map_err(|e| e.to_string())?;
            let texture = prepared
                .into_compressed_with_tags()
                .map_err(|e| e.to_string())?
                .0;
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
            // Verify output outside the timed region. Retain only the digest,
            // so successive samples have the same decode workspace available.
            let digest = texture.data().iter().fold(0xcbf29ce484222325u64, |h, &b| {
                (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
            });
            let identity = (texture.size, texture.format, texture.data().len(), digest);
            if expected.is_some_and(|old| old != identity) {
                return Err(format!("image output changed between samples: {name}"));
            }
            expected = Some(identity);
            if sample != 0 {
                times.push(elapsed_ms);
            }
            if sample == options.samples.get() {
                times.sort_by(f64::total_cmp);
                let median = (times[times.len() / 2] + times[(times.len() - 1) / 2]) * 0.5;
                rows.push(serde_json::json!({
                    "name": name, "encoded_bytes": encoded_bytes,
                    "native_bytes": texture.data().len(), "digest": format!("{digest:016x}"),
                    "samples": times.len(), "median_ms": median,
                    "budget_bytes": budget.limit(),
                    "min_ms": times[0], "max_ms": times[times.len() - 1],
                }));
            }
        }
    }
    let dropped = capture.finish()?;
    if dropped != 0 {
        return Err(format!("image recording lost {dropped} events"));
    }
    let json = serde_json::to_vec_pretty(&rows).map_err(|e| e.to_string())?;
    std::fs::write(options.out.join("images.json"), json).map_err(|e| e.to_string())?;
    super::report::generate(&options.out, 0., None)?;
    println!("Image measurements: {}", options.out.display());
    Ok(())
}
