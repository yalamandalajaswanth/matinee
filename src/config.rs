use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Parser;

/// Command line options.
#[derive(Debug, Parser)]
#[command(
    name = "matinee",
    version,
    about = "Stream the movies on your PC to DeoVR on Meta Quest over your home network"
)]
pub struct Cli {
    /// Library directory to scan (repeatable, at least one required).
    #[arg(long = "library", value_name = "DIR", required = true)]
    pub libraries: Vec<PathBuf>,

    /// TCP port to listen on.
    #[arg(long, default_value_t = 8080)]
    pub port: u16,

    /// Address to bind to.
    #[arg(long, default_value = "0.0.0.0")]
    pub bind: IpAddr,

    /// Directory for the library database (default: platform local data dir).
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,

    /// Do not print a QR code in the startup banner.
    #[arg(long)]
    pub no_qr: bool,
}

/// Validated runtime configuration.
#[derive(Debug)]
pub struct Config {
    pub libraries: Vec<PathBuf>,
    pub port: u16,
    pub bind: IpAddr,
    pub data_dir: PathBuf,
    pub no_qr: bool,
}

impl Cli {
    /// Validate the CLI input: skip missing libraries (with a warning) and resolve the data dir.
    pub fn into_config(self) -> Result<Config> {
        let mut libraries: Vec<PathBuf> = Vec::new();
        for lib in self.libraries {
            if !lib.is_dir() {
                tracing::warn!(
                    "library {} does not exist or is not a directory; skipping",
                    lib.display()
                );
                continue;
            }
            let abs = std::path::absolute(&lib).unwrap_or(lib);
            if !libraries.contains(&abs) {
                libraries.push(abs);
            }
        }
        if libraries.is_empty() {
            bail!(
                r#"no valid library folder was given. Pass one with --library, for example:
  matinee --library "D:\Movies"
(repeat --library to add more folders)"#
            );
        }
        let data_dir = match self.data_dir {
            Some(d) => d,
            None => directories::ProjectDirs::from("", "", "matinee")
                .map(|p| p.data_local_dir().to_path_buf())
                .context("could not determine a data directory; pass --data-dir")?,
        };
        Ok(Config {
            libraries,
            port: self.port,
            bind: self.bind,
            data_dir,
            no_qr: self.no_qr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_missing_and_errors_when_none_valid() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "x",
            "--library",
            dir.path().to_str().unwrap(),
            "--library",
            "Z:/definitely/not/here",
            "--data-dir",
            dir.path().to_str().unwrap(),
        ]);
        let cfg = cli.into_config().unwrap();
        assert_eq!(cfg.libraries.len(), 1);
        assert_eq!(cfg.port, 8080);

        let cli = Cli::parse_from(["x", "--library", "Z:/definitely/not/here"]);
        assert!(cli.into_config().is_err());
    }
}

/// Whether the program was started with no arguments at all, which on Windows usually
/// means a double-click from Explorer (the console would vanish on exit).
#[cfg(windows)]
pub fn is_bare_launch<I: IntoIterator>(args: I) -> bool {
    args.into_iter().nth(1).is_none()
}

#[cfg(all(test, windows))]
mod bare_launch_tests {
    use super::is_bare_launch;

    #[test]
    fn detects_no_arguments() {
        assert!(is_bare_launch(["matinee.exe"]));
        assert!(!is_bare_launch(["matinee.exe", "--help"]));
        assert!(!is_bare_launch(["matinee.exe", "--library", r"D:\Movies"]));
    }
}
