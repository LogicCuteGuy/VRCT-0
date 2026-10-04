use clap::Parser;
use std::path::PathBuf;
use vrct_whisper_eval::{
    evaluate::{run, Evaluate},
    Result,
};
#[derive(Parser)]
#[command(
    about = "Evaluate offline clips through VRCT's native Silero VAD and transcription storage"
)]
struct Args {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
    #[arg(long)]
    dataset_dir: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value = "clean")]
    condition: String,
    #[arg(long = "id")]
    item_id: Option<String>,
    #[arg(long,default_value="Whisper",value_parser=["Whisper","Google"])]
    engine: String,
    #[arg(long, default_value = "base")]
    model: String,
    #[arg(long, default_value = "int8")]
    compute_type: String,
    #[arg(long)]
    ort_library: Option<PathBuf>,
    #[arg(long)]
    all: bool,
}
fn main() {
    if let Err(error) = main_run() {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}
fn main_run() -> Result<()> {
    let a = Args::parse();
    let output = a.output.clone();
    let result = run(Evaluate {
        repo_root: a.repo_root,
        dataset_dir: a.dataset_dir,
        output: a.output,
        condition: a.condition,
        item_id: a.item_id,
        engine: a.engine,
        model: a.model,
        compute_type: a.compute_type,
        ort_library: a.ort_library,
        all: a.all,
    })?;
    println!(
        "VRCT transcription test passed={} output={}",
        result["passed"],
        output.display()
    );
    if result["passed"] != true {
        std::process::exit(1);
    }
    Ok(())
}
