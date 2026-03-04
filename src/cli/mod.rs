use clap::{
    builder::{
        styling::{AnsiColor, Effects},
        Styles,
    },
    Parser, Subcommand,
};

mod describe;
mod dump;
mod filter;
mod input;
mod prefetch;
mod recode;
mod runtime;
pub use describe::{DescribeArgs, DescribeOptions};
pub use dump::{DumpArgs, DumpOutput, OutputFormat};
pub use filter::FilterOptions;
pub use input::{AccessionOptions, InputOptions, MultiInputOptions, Provider};
pub use prefetch::PrefetchArgs;
pub use recode::RecodeArgs;
pub use runtime::RuntimeOptions;

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Yellow.on_default());

#[derive(Parser, Debug)]
#[command(styles = STYLES, version)]
pub struct Cli {
    #[clap(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Extract the records of the SRA file and output them as FASTQ
    #[clap(name = "dump")]
    Dump(DumpArgs),

    /// Recode the records of the SRA file as BINSEQ
    #[clap(name = "recode")]
    Recode(RecodeArgs),

    /// Describe the read segments in the SRA file within a specified limit
    #[clap(name = "describe")]
    Describe(DescribeArgs),

    /// Downloads an SRA file to disk
    #[clap(name = "prefetch")]
    Prefetch(PrefetchArgs),
}
