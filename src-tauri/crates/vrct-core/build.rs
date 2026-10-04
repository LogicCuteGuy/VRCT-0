fn main() {
    println!("cargo:rerun-if-changed=src/audio/asio_panel.cpp");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        cc::Build::new().cpp(true).file("src/audio/asio_panel.cpp").compile("vrct_asio_panel");
    }
}
