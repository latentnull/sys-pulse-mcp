mod tools;

use std::{env, error::Error, ffi::OsString, path::PathBuf};

use rmcp::{ServiceExt, transport::stdio};
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
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CliOptions {
    allow_dirs: Option<Vec<PathBuf>>,
    show_help: bool,
}

fn parse_cli_args(args: impl IntoIterator<Item = OsString>) -> Result<CliOptions, String> {
    let mut args = args.into_iter();
    let mut options = CliOptions::default();

    while let Some(argument) = args.next() {
        if argument == "--help" || argument == "-h" {
            options.show_help = true;
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
                show_help: false,
            }
        );
    }

    #[test]
    fn cli_rejects_missing_allow_dir_value_and_unknown_flags() {
        assert!(parse_cli_args([OsString::from("--allow-dir")]).is_err());
        assert!(parse_cli_args([OsString::from("--unexpected")]).is_err());
    }
}
