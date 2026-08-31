//! Phase 4 真机验收入口。默认只做 dry-run，真实开流需双重显式授权。

use std::sync::Arc;

use hse_service::backend::WasapiFactory;
use hse_service::real_audio_check;

fn main() {
    let code = real_audio_check::run_cli(std::env::args().skip(1), Arc::new(WasapiFactory));
    std::process::exit(code);
}
