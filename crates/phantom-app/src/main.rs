//! Phantom Terminal binary — a thin wrapper over the `phantom_app` library so
//! the app logic stays a testable library.

fn main() {
    match phantom_app::cli::dispatch_from_env() {
        Ok(true) => {}
        Ok(false) => {
            if let Err(error) = phantom_app::run() {
                eprintln!("phantom: {error}");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("phantom: {error}");
            std::process::exit(error.exit_code());
        }
    }
}
