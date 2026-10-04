use clap::Parser;
fn main() {
    if let Err(error) = vrct_capture_tools::probe::run(vrct_capture_tools::probe::Args::parse()) {
        eprintln!("[ERROR] {error}");
        std::process::exit(1);
    }
}
