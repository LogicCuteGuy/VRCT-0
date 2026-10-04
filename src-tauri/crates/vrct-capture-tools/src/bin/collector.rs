use clap::Parser;
fn main() {
    let result = vrct_capture_tools::collector::run(vrct_capture_tools::collector::Args::parse());
    if let Err(error) = &result {
        eprintln!("[ERROR] {error}");
    }
    vrct_capture_tools::console::finish_default_launch();
    if result.is_err() {
        std::process::exit(1);
    }
}
