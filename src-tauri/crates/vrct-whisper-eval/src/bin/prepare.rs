use clap::Parser;
use std::path::PathBuf;
use vrct_whisper_eval::{
    dataset::{prepare_dataset, Prepare},
    Result,
};
#[derive(Parser)]
#[command(
    about = "Prepare seeded, stratified Common Voice clips and noise variants without Python"
)]
struct Args {
    #[arg(long)]
    input_dir: PathBuf,
    #[arg(long, default_value = "validated.tsv")]
    tsv: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long)]
    environment_noise: PathBuf,
    #[arg(long, default_value_t = 150)]
    count: usize,
    #[arg(long, default_value_t = 20260911, allow_hyphen_values = true)]
    seed: i64,
    #[arg(long, default_value = "ffmpeg")]
    ffmpeg: String,
    #[arg(long, default_value = "Mozilla Common Voice Japanese")]
    source_name: String,
    #[arg(long, default_value = "https://commonvoice.mozilla.org/en/datasets")]
    source_url: String,
    #[arg(long, default_value = "CC0-1.0")]
    license: String,
    #[arg(long, default_value = "")]
    dataset_version: String,
}
fn run() -> Result<()> {
    let a = Args::parse();
    if !(100..=200).contains(&a.count) {
        return Err("--count must be between100 and200 for planned evaluation set".into());
    }
    let config = Prepare {
        tsv: a.input_dir.join(a.tsv),
        input_dir: a.input_dir,
        output_dir: a.output_dir,
        environment_noise: a.environment_noise,
        count: a.count,
        seed: a.seed,
        ffmpeg: a.ffmpeg,
        source_name: a.source_name,
        source_url: a.source_url,
        license: a.license,
        dataset_version: a.dataset_version,
    };
    let result = prepare_dataset(&config)?;
    println!(
        "Prepared {} utterances at {}",
        result["count"],
        config.output_dir.display()
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}
