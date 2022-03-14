//! Helper tasks for the `apecs` project.
//!
//! Run `cargo xtask help` for more info.
use clap::{Parser, Subcommand};
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

mod bench;
use bench::Bench;

#[derive(Parser)]
#[clap(author, version, about, subcommand_required = true)]
struct Cli {
    /// Sets the verbosity level
    #[clap(short, parse(from_occurrences))]
    verbosity: usize,
    /// The task to run
    #[clap(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run benchmarks
    Bench(Bench),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let level = match cli.verbosity {
        0 => Level::WARN,
        1 => Level::INFO,
        2 => Level::DEBUG,
        _ => Level::TRACE,
    };
    // use the verbosity level later when we build TVM
    let subscriber = FmtSubscriber::builder().with_max_level(level).finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    match cli.command {
        Command::Bench(bench) => bench.run()?,
    }

    Ok(())
}
