mod bridge_client;
mod server;

use anyhow::{anyhow, bail, Context, Result};
use rmcp::{transport::stdio, ServiceExt};
use server::RenderlyMcp;
use std::path::PathBuf;
use tracing_subscriber::{fmt, EnvFilter};

/// Parses the optional `--project <path>` startup argument. Anything else is an error, so a
/// typo cannot silently start a headless server with no project.
fn parse_project_arg(args: impl IntoIterator<Item = String>) -> Result<Option<PathBuf>> {
    let mut args = args.into_iter();
    let mut project = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => {
                let value = args.next().context("--project needs a path argument")?;
                project = Some(PathBuf::from(value));
            }
            other => bail!("unknown argument `{other}` (usage: renderly-mcp [--project <path>])"),
        }
    }
    Ok(project)
}

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let project = parse_project_arg(std::env::args().skip(1))?;
    let server = RenderlyMcp::new();
    if let Some(path) = project {
        // Same as the open_project tool, but a bad path is a launch error rather than a silent
        // headless session that every later edit would trip over.
        server
            .open_project_at_startup(path.clone())
            .map_err(|e| anyhow!("--project {}: {e}", path.display()))?;
        tracing::info!("renderly-mcp opened project {}", path.display());
    }

    tracing::info!("renderly-mcp starting (stdio)");
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_project_arg;
    use std::path::PathBuf;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_means_no_project() {
        assert_eq!(parse_project_arg(args(&[])).unwrap(), None);
    }

    #[test]
    fn project_flag_takes_a_path() {
        assert_eq!(
            parse_project_arg(args(&["--project", "C:/p/edit.renderly.json"])).unwrap(),
            Some(PathBuf::from("C:/p/edit.renderly.json"))
        );
    }

    #[test]
    fn project_flag_without_value_is_an_error() {
        assert!(parse_project_arg(args(&["--project"])).is_err());
    }

    #[test]
    fn unknown_argument_is_an_error() {
        assert!(parse_project_arg(args(&["--proejct", "x"])).is_err());
    }
}
