use clap::{Parser, Subcommand};
use std::path::PathBuf;
use vrct_annotator::{
    dataset::{self, Options},
    Result,
};

#[derive(Parser)]
#[command(
    name = "vrct-dataset",
    about = "Reviewed labels -> frozen scene-stratified YOLO splits -> COCO"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Refresh labels and lists. Empty reviewed labels remain negative samples.
    #[command(alias = "yolo")]
    Prepare {
        #[arg(long, default_value = "dataset_annotated")]
        root: PathBuf,
        #[arg(long, default_value_t = 0.2)]
        val_ratio: f64,
        #[arg(long, default_value_t = 10)]
        scene_size: u64,
        #[arg(long, default_value_t = 0, allow_negative_numbers = true)]
        seed: i64,
        #[arg(long)]
        refreeze: bool,
    },
    /// Convert the existing train/val listings into COCO without resplitting.
    #[command(alias = "yolox")]
    Coco {
        #[arg(long, default_value = "dataset_annotated")]
        root: PathBuf,
    },
}
fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Prepare {
            root,
            val_ratio,
            scene_size,
            seed,
            refreeze,
        } => {
            let summary = dataset::prepare(
                &root,
                &Options {
                    val_ratio,
                    scene_size,
                    seed,
                    refreeze,
                },
            )?;
            println!(
                "train {} / val {} / {} positive / {} negative -> {}",
                summary.train,
                summary.val,
                summary.positives,
                summary.negatives,
                root.join("data.yaml").display()
            );
        }
        Command::Coco { root } => {
            for path in dataset::coco(&root)? {
                println!("{}", path.display());
            }
        }
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("[ERROR] {error}");
        std::process::exit(1);
    }
}
