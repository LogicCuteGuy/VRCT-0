use clap::{Parser, Subcommand};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use vrct_annotator::{
    gemini::{self, AnnotateOptions},
    job, Result,
};

#[derive(Parser)]
#[command(
    name = "vrct-annotator",
    about = "Collector PNG snapshots -> Gemini predictions -> Label Studio human review"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Offline: validate the native REST client/schema without sending a request.
    Check,
    /// Offline: snapshot collector PNG/JSON pairs into a NEW external job.
    Prepare {
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long, default_value_t = 42, allow_negative_numbers = true)]
        seed: i64,
        #[arg(long,default_value=job::DEFAULT_MODEL)]
        model: String,
    },
    /// Send pending images to Gemini. Credentials come only from GEMINI_API_KEY.
    Annotate {
        job: PathBuf,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long, default_value_t = 6.)]
        interval: f64,
        #[arg(long, default_value_t = 2)]
        retries: u8,
        #[arg(long)]
        retry_failed: bool,
    },
    /// Offline: create an additive Label Studio predictions snapshot.
    Export { job: PathBuf },
    /// Offline: print states and reported token usage from all attempts.
    Status { job: PathBuf },
}
fn print_summary(summary: &serde_json::Value) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(summary).map_err(|_| "cannot serialize summary")?
    );
    Ok(())
}
fn exit_status(summary: &serde_json::Value) -> i32 {
    if summary["counts"]
        .as_object()
        .is_some_and(|counts| counts.keys().all(|status| job::success(status)))
    {
        0
    } else {
        2
    }
}
async fn process(job_path: &Path, key: Option<&str>, options: &AnnotateOptions) -> Result<i32> {
    let result = tokio::select! {
        result = gemini::annotate(job_path,key,options) => Some(result),
        result = tokio::signal::ctrl_c() => { result.map_err(|_| "cannot install Ctrl+C handler")?; None }
    };
    match result {
        Some(result) => {
            let summary = result?;
            print_summary(&summary)?;
            Ok(exit_status(&summary))
        }
        None => {
            let destination = job::export(job_path)?;
            println!(
                "Interrupted; saved results will be reused. Label Studio: {}",
                destination.display()
            );
            Ok(130)
        }
    }
}
fn ask(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut value = String::new();
    if io::stdin()
        .read_line(&mut value)
        .map_err(|e| e.to_string())?
        == 0
    {
        return Err("input ended".into());
    }
    Ok(value.trim().trim_matches('"').to_owned())
}
async fn wizard() -> Result<i32> {
    if !io::stdin().is_terminal() {
        return Err(
            "Use prepare / annotate / export / status / check in a non-interactive terminal".into(),
        );
    }
    println!("VRCT Gemini Annotator\nCreate a snapshot job and provisional predictions. Resume by entering the existing job folder. API keys are never saved. Ctrl+C stops processing.");
    let entered = ask("Collector image folder or existing job folder: ")?;
    if entered.is_empty() {
        return Err("Enter an image folder or job folder".into());
    }
    let input = PathBuf::from(entered);
    let path = if input.join("manifest.json").is_file() {
        input
    } else {
        let entered = ask("Sample size [100 / 0=all]: ")?;
        let limit = if entered.is_empty() {
            100
        } else {
            entered.parse().map_err(|_| "invalid sample size")?
        };
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let path = executable
            .parent()
            .ok_or("executable directory unavailable")?
            .join("annotation_jobs")
            .join(job::unique_stamp()?);
        let manifest = job::prepare(&input, &path, limit, 42, job::DEFAULT_MODEL)?;
        println!(
            "Prepared {}/{} images: {}",
            manifest.images.len(),
            manifest.available_images,
            path.display()
        );
        path
    };
    let destination = job::export(&path)?;
    print_summary(&job::status(&path)?)?;
    println!("Label Studio: {}", destination.display());
    let action = ask("1=process pending / 2=retry failed/unknown / Enter=prepare only: ")?;
    if !matches!(action.as_str(), "1" | "2") {
        return Ok(0);
    }
    println!(
        "Selected images will be sent to Gemini API. Model: {}",
        job::load(&path)?.model
    );
    let key = std::env::var("GEMINI_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .map(Ok)
        .unwrap_or_else(|| {
            rpassword::prompt_password("Gemini API key (hidden): ")
                .map_err(|_| "cannot read hidden API key".to_owned())
        })?;
    process(
        &path,
        Some(&key),
        &AnnotateOptions {
            limit: 0,
            retry_failed: action == "2",
            ..Default::default()
        },
    )
    .await
}
async fn run() -> Result<i32> {
    match Cli::parse().command {
        None => wizard().await,
        Some(Command::Check) => {
            if !job::valid_model(job::DEFAULT_MODEL)
                || gemini::request_body(b"offline-check")["generationConfig"]["responseJsonSchema"]
                    != job::schema()
            {
                return Err("native Gemini client/schema check failed".into());
            }
            reqwest::Client::builder()
                .http1_only()
                .pool_max_idle_per_host(0)
                .build()
                .map_err(|_| "native HTTP initialization failed")?;
            println!("Gemini REST client and schema: OK (no request sent)");
            Ok(0)
        }
        Some(Command::Prepare {
            input,
            out,
            limit,
            seed,
            model,
        }) => {
            let manifest = job::prepare(&input, &out, limit, seed, &model)?;
            let destination = job::export(&out)?;
            println!(
                "Prepared {}/{} images: {}\nLabel Studio: {}",
                manifest.images.len(),
                manifest.available_images,
                out.display(),
                destination.display()
            );
            Ok(0)
        }
        Some(Command::Annotate {
            job,
            limit,
            interval,
            retries,
            retry_failed,
        }) => {
            let key = std::env::var("GEMINI_API_KEY").ok();
            process(
                &job,
                key.as_deref(),
                &AnnotateOptions {
                    limit,
                    interval,
                    retries,
                    retry_failed,
                    endpoint: None,
                },
            )
            .await
        }
        Some(Command::Export { job: path }) => {
            println!("{}", job::export(&path)?.display());
            print_summary(&job::status(&path)?)?;
            Ok(0)
        }
        Some(Command::Status { job: path }) => {
            print_summary(&job::status(&path)?)?;
            Ok(0)
        }
    }
}
#[tokio::main]
async fn main() {
    let code = match run().await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("[ERROR] {error}");
            1
        }
    };
    std::process::exit(code);
}
