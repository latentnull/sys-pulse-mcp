mod telemetry;
mod tools;

use std::{
    env,
    error::Error,
    ffi::OsString,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

use rmcp::{ServiceExt, transport::stdio};
use telemetry::{SampleHistory, run_sampler};
use tools::SysPulseServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init()?;

    tracing::info!("Starting sys-pulse-mcp over stdio");
    let options = parse_cli_args(env::args_os().skip(1))
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    if options.show_help {
        eprintln!(
            "sys-pulse-mcp [--allow-dir <path>]...\n\
             \n\
             --allow-dir <path>  Allow repositories beneath this directory (repeatable).\n\
             --sample-interval-secs <seconds>  Set telemetry sampling interval (default 2).\n\
             When omitted, SYS_PULSE_ALLOWED_BASES is used, then the current directory."
        );
        return Ok(());
    }

    let server = match options.allow_dirs {
        Some(allow_dirs) => {
            SysPulseServer::with_allowed_bases(allow_dirs).map_err(std::io::Error::other)?
        }
        None => SysPulseServer::new().map_err(std::io::Error::other)?,
    };
    let history = Arc::new(RwLock::new(SampleHistory::default()));
    let sampler = tokio::spawn(run_sampler(
        Arc::clone(&history),
        Duration::from_secs(options.sample_interval_secs),
    ));
    let server = server.with_sample_history(history);
    let service = server.serve(stdio()).await?;
    let result = service.waiting().await;
    sampler.abort();
    result?;
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CliOptions {
    allow_dirs: Option<Vec<PathBuf>>,
    sample_interval_secs: u64,
    show_help: bool,
}

fn parse_cli_args(args: impl IntoIterator<Item = OsString>) -> Result<CliOptions, String> {
    let mut args = args.into_iter();
    let sample_interval_secs = match env::var_os("SYS_PULSE_SAMPLE_INTERVAL_SECS") {
        Some(value) => parse_sample_interval(&value)?,
        None => 2,
    };
    let mut options = CliOptions {
        sample_interval_secs,
        ..CliOptions::default()
    };

    while let Some(argument) = args.next() {
        if argument == "--help" || argument == "-h" {
            options.show_help = true;
            continue;
        }

        if argument == "--sample-interval-secs" {
            let value = args
                .next()
                .ok_or_else(|| "--sample-interval-secs requires a value.".to_owned())?;
            options.sample_interval_secs = parse_sample_interval(&value)?;
            continue;
        }

        if argument == "--allow-dir" {
            let path = args
                .next()
                .ok_or_else(|| "--allow-dir requires a directory path.".to_owned())?;
            options
                .allow_dirs
                .get_or_insert_with(Vec::new)
                .push(PathBuf::from(path));
            continue;
        }

        if let Some(value) = argument
            .to_str()
            .and_then(|value| value.strip_prefix("--sample-interval-secs="))
        {
            options.sample_interval_secs = parse_sample_interval(&OsString::from(value))?;
            continue;
        }

        if let Some(path) = argument
            .to_str()
            .and_then(|value| value.strip_prefix("--allow-dir="))
        {
            if path.is_empty() {
                return Err("--allow-dir requires a non-empty directory path.".to_owned());
            }
            options
                .allow_dirs
                .get_or_insert_with(Vec::new)
                .push(PathBuf::from(path));
            continue;
        }

        return Err(format!(
            "Unknown command-line argument: {}",
            argument.to_string_lossy()
        ));
    }

    Ok(options)
}

fn parse_sample_interval(value: &OsString) -> Result<u64, String> {
    let interval = value
        .to_str()
        .ok_or_else(|| "Sample interval must be valid UTF-8.".to_owned())?
        .parse::<u64>()
        .map_err(|_| "Sample interval must be an integer from 1 to 3600.".to_owned())?;
    if !(1..=3600).contains(&interval) {
        return Err("Sample interval must be an integer from 1 to 3600.".to_owned());
    }
    Ok(interval)
}

#[cfg(test)]
mod tests {
    use super::{CliOptions, parse_cli_args};
    use std::{ffi::OsString, path::PathBuf};

    #[test]
    fn cli_parses_multiple_allow_dirs() {
        let options = parse_cli_args([
            OsString::from("--allow-dir"),
            OsString::from("C:\\Work\\Projects"),
            OsString::from("--allow-dir=D:\\Repositories"),
        ])
        .expect("valid allow-dir arguments parse");

        assert_eq!(
            options,
            CliOptions {
                allow_dirs: Some(vec![
                    PathBuf::from("C:\\Work\\Projects"),
                    PathBuf::from("D:\\Repositories"),
                ]),
                sample_interval_secs: 2,
                show_help: false,
            }
        );
    }

    #[test]
    fn cli_rejects_missing_allow_dir_value_and_unknown_flags() {
        assert!(parse_cli_args([OsString::from("--allow-dir")]).is_err());
        assert!(parse_cli_args([OsString::from("--unexpected")]).is_err());
    }

    #[test]
    fn cli_parses_and_validates_sampling_interval() {
        let options = parse_cli_args([
            OsString::from("--sample-interval-secs=5"),
            OsString::from("--allow-dir=C:\\Work\\Projects"),
        ])
        .expect("valid interval should parse");
        assert_eq!(options.sample_interval_secs, 5);
        assert!(
            parse_cli_args([
                OsString::from("--sample-interval-secs"),
                OsString::from("0")
            ])
            .is_err()
        );
    }
}
