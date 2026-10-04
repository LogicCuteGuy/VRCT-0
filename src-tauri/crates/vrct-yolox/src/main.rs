use clap::{Parser, Subcommand};
use std::path::PathBuf;
use vrct_yolox::{
    evaluation, network, quantization,
    training::{self, TrainOptions},
    Result,
};

#[derive(Parser)]
#[command(about = "Native YOLOX training, ONNX export, calibration and detection evaluation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Train {
        #[arg(long, default_value = "dataset_annotated")]
        root: PathBuf,
        #[arg(short = 'o', long, default_value = "runs/chatbox_yolox_native")]
        output: PathBuf,
        #[arg(long,default_value="tiny",value_parser=["tiny","nano"])]
        variant: String,
        #[arg(short = 'c', long = "ckpt")]
        checkpoint: Option<PathBuf>,
        #[arg(long, conflicts_with = "checkpoint")]
        from_scratch: bool,
        #[arg(long, requires = "checkpoint")]
        resume: bool,
        #[arg(long, default_value_t = 80)]
        epochs: usize,
        #[arg(short = 'b', long, default_value_t = 8)]
        batch_size: usize,
        #[arg(long, default_value_t = 1280)]
        size: usize,
        #[arg(long, default_value_t = 0)]
        seed: u64,
        #[arg(long, default_value_t = 0.001)]
        lr: f64,
        #[arg(long, default_value_t = 5)]
        warmup_epochs: usize,
        #[arg(long, default_value_t = 20)]
        no_aug_epochs: usize,
        #[arg(long, default_value_t = 5)]
        eval_interval: usize,
        #[arg(long,default_value="cpu",value_parser=["cpu","cuda"])]
        device: String,
        #[arg(long, conflicts_with = "no_fp16")]
        fp16: bool,
        #[arg(long, conflicts_with = "fp16")]
        no_fp16: bool,
        #[arg(long, default_value_t = 5)]
        multiscale_range: usize,
    },
    Export {
        #[arg(short = 'c', long = "ckpt")]
        checkpoint: PathBuf,
        #[arg(short = 'o', long)]
        output: PathBuf,
        #[arg(long,default_value="tiny",value_parser=["tiny","nano"])]
        variant: String,
        #[arg(long, default_value = "1280,1280")]
        size: String,
        #[arg(long)]
        dynamic: bool,
    },
    Quantize {
        #[arg(short = 'i', long)]
        input: PathBuf,
        #[arg(short = 'o', long)]
        output: PathBuf,
        #[arg(long, default_value = "dataset_annotated")]
        root: PathBuf,
        #[arg(long, default_value = "736,1280")]
        size: String,
        #[arg(long, default_value = "train")]
        split: String,
        #[arg(long, default_value_t = 4)]
        every: usize,
    },
    Eval {
        #[arg(long)]
        model: PathBuf,
        #[arg(long, default_value = "dataset_annotated")]
        root: PathBuf,
        #[arg(long, default_value = "val")]
        split: String,
        #[arg(long, default_value = "1280")]
        imgsz: String,
        #[arg(long, default_value_t = 0.15)]
        conf: f32,
        #[arg(long, default_value_t = 0.65)]
        iou: f32,
        #[arg(long, default_value_t = 0.5)]
        match_iou: f32,
        #[arg(long)]
        coco_eval: bool,
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
}
fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Train {
            root,
            output,
            variant,
            checkpoint,
            from_scratch,
            resume,
            epochs,
            batch_size,
            size,
            seed,
            lr,
            warmup_epochs,
            no_aug_epochs,
            eval_interval,
            device,
            fp16,
            no_fp16,
            multiscale_range,
        } => {
            if !from_scratch && checkpoint.is_none() {
                return Err(
                    "provide authorized --ckpt pretrained weights or explicitly use --from-scratch"
                        .into(),
                );
            }
            let fp16 = fp16 || device == "cuda" && !no_fp16;
            training::train(TrainOptions {
                root,
                output,
                variant,
                checkpoint,
                resume,
                epochs,
                batch_size,
                size,
                seed,
                lr,
                warmup_epochs,
                no_aug_epochs,
                eval_interval,
                device,
                fp16,
                multiscale_range,
            })
        }
        Command::Export {
            checkpoint,
            output,
            variant,
            size,
            dynamic,
        } => {
            let dims = evaluation::parse_size(&size)?;
            if dims.len() != 2 {
                return Err("export --size requires H,W".into());
            }
            let net = network::build(&variant, candle_core::Device::Cpu, 0)?;
            let checkpoint = if checkpoint.is_dir() {
                checkpoint.join("weights.safetensors")
            } else {
                checkpoint
            };
            vrct_yolox::graph::protect_input(&output, &checkpoint)?;
            net.load(&checkpoint, false)?;
            net.export(&output, dims[0], dims[1], dynamic)?;
            println!(
                "Wrote {}: native YOLOX {variant}, decoded [1,N,6], opset 17",
                output.display()
            );
            Ok(())
        }
        Command::Quantize {
            input,
            output,
            root,
            size,
            split,
            every,
        } => {
            let dims = evaluation::parse_size(&size)?;
            if dims.len() != 2 {
                return Err("quantize --size requires H,W".into());
            }
            quantization::quantize(&input, &output, &root, &split, dims[0], dims[1], every)
        }
        Command::Eval {
            model,
            root,
            split,
            imgsz,
            conf,
            iou,
            match_iou,
            coco_eval,
            output,
        } => {
            if let Some(output) = &output {
                vrct_yolox::graph::protect_input(output, &model)?;
                vrct_yolox::graph::protect_input(output, &root.join(format!("{split}.txt")))?;
                for path in training::entries(&root, &split)? {
                    vrct_yolox::graph::protect_input(output, &path)?;
                    if let (Some(session), Some(stem)) = (
                        path.parent().and_then(std::path::Path::parent),
                        path.file_stem(),
                    ) {
                        vrct_yolox::graph::protect_input(
                            output,
                            &session.join("labels").join(stem).with_extension("txt"),
                        )?;
                    }
                }
            }
            let report =
                evaluation::evaluate_onnx(&model, &root, &split, &imgsz, conf, iou, match_iou)?;
            if coco_eval {
                println!(
                    "COCO-style one-class AP50:95 {:.5}, AP50 {:.5}, AP75 {:.5}",
                    report.ap50_95, report.ap50, report.ap75
                );
            }
            let bytes = serde_json::to_vec_pretty(&report)?;
            println!("{}", String::from_utf8_lossy(&bytes));
            if let Some(output) = output {
                vrct_yolox::graph::atomic_write(&output, &bytes)?;
            }
            Ok(())
        }
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!("detector tool failed: {e}");
        std::process::exit(1);
    }
}
