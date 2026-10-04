use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("native task failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut profile = "debug".to_owned();
    let mut output = None;
    let mut offline = false;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--root" => {
                root = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or("--root needs a path")?
            }
            "--profile" => profile = args.next().ok_or("--profile needs debug or release")?,
            "--output" => {
                output = Some(
                    args.next()
                        .map(PathBuf::from)
                        .ok_or("--output needs a path")?,
                )
            }
            "--offline" => offline = true,
            _ => return Err(format!("unknown argument {argument}")),
        }
    }
    let root = std::fs::canonicalize(root).map_err(|e| format!("repository unavailable: {e}"))?;
    match command.as_str() {
        "prepare" => xtask::prepare(&root, &profile, offline),
        "version" => xtask::sync_version(&root),
        "tools" => xtask::build_tools(&root, &profile),
        "package-tools" => {
            let output = output.unwrap_or_else(|| root.join("tool-dist/VRCT-native-tools.zip"));
            let path = xtask::package_tools(&root, &profile, &output)?;
            println!("Created and verified {}", path.display());
            Ok(())
        }
        "package" => {
            let output = output.unwrap_or_else(|| root.join("VRCT-0.zip"));
            let path = xtask::package(&root, &profile, &output)?;
            println!("Created and verified {}", path.display());
            Ok(())
        }
        "verify" => {
            xtask::verify_zip(&output.unwrap_or_else(|| root.join("VRCT-0.zip")))?;
            println!("Native ZIP verified");
            Ok(())
        }
        "help" => {
            println!("cargo run -p xtask -- prepare|version|package|verify|tools|package-tools [--profile debug|release] [--offline] [--output path] [--root path]");
            Ok(())
        }
        _ => Err(format!("unknown task {command}")),
    }
}
