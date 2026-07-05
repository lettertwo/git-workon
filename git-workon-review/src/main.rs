use clap::Parser;

/// A TUI for reviewing changesets
#[derive(Debug, Parser)]
#[clap(
    about,
    author,
    bin_name = env!("CARGO_PKG_NAME"),
    version,
    arg_required_else_help = true
)]
struct Cli {}

fn main() -> miette::Result<()> {
    Cli::parse();

    Ok(())
}
