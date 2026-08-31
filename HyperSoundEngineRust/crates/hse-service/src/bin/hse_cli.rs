//! hse-cli bin 入口：转调 hse_service::cli。

fn main() {
    let code = hse_service::cli::run(std::env::args().skip(1));
    std::process::exit(code);
}
