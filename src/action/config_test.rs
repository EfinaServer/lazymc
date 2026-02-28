use std::path::PathBuf;

use clap::ArgMatches;

use crate::config::{self, Config};
use crate::util::error::{quit_error, quit_error_msg, ErrorHintsBuilder};

/// Invoke config test command.
pub fn invoke(matches: &ArgMatches) {
    // Get config path, attempt to canonicalize
    let mut path = PathBuf::from(matches.get_one::<String>("config").unwrap());
    if let Ok(p) = path.canonicalize() {
        path = p;
    }

    if path.is_file() {
        // Config file exists — load and test it (with env overrides)
        let _config = match Config::load(path) {
            Ok(config) => config,
            Err(err) => {
                quit_error(
                    anyhow!(err).context("Failed to load and parse config"),
                    ErrorHintsBuilder::default().build().unwrap(),
                );
            }
        };

        eprintln!("Config loaded successfully!");
    } else if config::has_env_config() {
        // No config file, but LAZYMC_ env vars present — test env-only config
        let _config = match Config::from_env() {
            Ok(config) => config,
            Err(err) => {
                quit_error(
                    anyhow!(err).context("Failed to load config from environment variables"),
                    ErrorHintsBuilder::default().build().unwrap(),
                );
            }
        };

        eprintln!("Config loaded successfully from environment variables!");
    } else {
        quit_error_msg(
            format!(
                "Config file does not exist at: {}\n\
                 Hint: you can also configure lazymc entirely through LAZYMC_ environment variables.",
                path.to_str().unwrap_or("?")
            ),
            ErrorHintsBuilder::default().build().unwrap(),
        );
    }

    // TODO: do additional config tests: server dir correct, command set?
}
