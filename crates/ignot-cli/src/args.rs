//! The command line's grammar.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

const WHEN: &str = "WHEN is a time (2026-12-03T14:00:00Z) or a delay from now (+90m, +6h, +2d).";

/// Seal an exam for AI models until a public clock opens it, then check every claim about it.
#[derive(Debug, Parser)]
#[command(name = "ignot", version, propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Make an exam from a seed. The file holds the keys: seal it, never publish it.
    Make {
        /// A short name: lowercase letters, digits and dashes.
        #[arg(long)]
        id: String,
        /// The name shown to people. Defaults to the id.
        #[arg(long)]
        title: Option<String>,
        /// The secret the questions are drawn from. Defaults to 32 random bytes.
        #[arg(long)]
        seed: Option<String>,
        /// What to ask, as kind:level or kind:level:count, separated by commas. Defaults to every kind at levels 3 to 5.
        #[arg(long)]
        mix: Option<String>,
        /// The commit this generator was built from. Defaults to the commit checked out here.
        #[arg(long)]
        commit: Option<String>,
        /// Where to write the exam. Defaults to ID.plain.json.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Seal an exam until a time. Writes exam.age and manifest.json into a folder.
    #[command(after_help = WHEN)]
    Seal {
        /// The file `make` wrote.
        exam: PathBuf,
        /// When the exam opens.
        #[arg(long, value_name = "WHEN")]
        open: String,
        /// The folder to write into. Defaults to exams/ID.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Keep the unsealed file instead of deleting it.
        #[arg(long)]
        keep: bool,
    },
    /// Open a sealed exam once its round has passed. Writes exam.json.
    Open {
        /// The exam's folder.
        dir: PathBuf,
    },
    /// Put an opened exam to models and score them. Reads the API key from OPENROUTER_API_KEY.
    ///
    /// Run it again to add models or a track, or to finish a run that was cut short or had failed requests: an
    /// answer that is in already is never asked for again. To ask a model again from scratch, remove its file
    /// from the exam's answers folder first.
    Run {
        /// The exam's folder.
        dir: PathBuf,
        /// Models to ask, by their OpenRouter names, separated by commas.
        #[arg(long, value_delimiter = ',', required_unless_present = "panel")]
        models: Vec<String>,
        /// A JSON file holding a list of model names.
        #[arg(long, conflicts_with = "models")]
        panel: Option<PathBuf>,
        /// Which track to run.
        #[arg(long, value_enum, default_value_t = Tracks::Both)]
        track: Tracks,
        /// Dollars per question on the same-budget track.
        #[arg(long, default_value = "0.02", value_name = "USD")]
        budget: String,
        /// How hard the models are asked to think, for the questions this run asks.
        #[arg(long, value_enum, default_value_t = Effort::High)]
        effort: Effort,
        /// Questions in flight at once.
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=64))]
        concurrency: u16,
    },
    /// Check an exam's folder: the seal, the opened exam, the answers, the scores, the record on chain.
    Verify {
        /// The exam's folder.
        dir: PathBuf,
        #[command(flatten)]
        chain: ChainArgs,
    },
    /// Check every exam under a folder.
    VerifyAll {
        /// The folder that holds one folder per exam.
        #[arg(default_value = "exams")]
        root: PathBuf,
        #[command(flatten)]
        chain: ChainArgs,
    },
    /// Seal any file until a time.
    #[command(after_help = WHEN)]
    SealFile {
        /// The file to seal.
        file: PathBuf,
        /// When it opens.
        #[arg(long, value_name = "WHEN")]
        open: String,
        /// Where to write the sealed file. Defaults to FILE.age.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Open a sealed file once its round has passed.
    OpenFile {
        /// The sealed file.
        file: PathBuf,
        /// Where to write the opened file. Defaults to the name without .age.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, clap::Args)]
pub struct ChainArgs {
    /// The node to read receipts from. Defaults to the public node of the chain the manifest names.
    #[arg(long, value_name = "URL")]
    pub rpc: Option<String>,
    /// Do not read the chain. The on-chain checks are then reported as skipped.
    #[arg(long, conflicts_with = "rpc")]
    pub no_chain: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Tracks {
    Both,
    SameBudget,
    Unlimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grammar_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn run_needs_models_or_a_panel_and_not_both() {
        assert!(Cli::try_parse_from(["ignot", "run", "exams/x"]).is_err());
        assert!(Cli::try_parse_from(["ignot", "run", "exams/x", "--models", "a/b", "--panel", "p.json"]).is_err());
        let cli =
            Cli::try_parse_from(["ignot", "run", "exams/x", "--models", "a/b,c/d", "--track", "same-budget"]).unwrap();
        let Command::Run { models, track, budget, effort, concurrency, .. } = cli.command else { panic!("not run") };
        assert_eq!(
            (models, track, budget.as_str(), effort, concurrency),
            (vec!["a/b".to_string(), "c/d".to_string()], Tracks::SameBudget, "0.02", Effort::High, 4)
        );
        assert!(Cli::try_parse_from(["ignot", "run", "x", "--models", "a", "--concurrency", "0"]).is_err());
        assert!(Cli::try_parse_from(["ignot", "run", "x", "--models", "a", "--effort", "extreme"]).is_err());
    }

    #[test]
    fn verify_reads_the_chain_unless_told_not_to() {
        let cli = Cli::try_parse_from(["ignot", "verify-all"]).unwrap();
        let Command::VerifyAll { root, chain } = cli.command else { panic!("not verify-all") };
        assert_eq!((root, chain.rpc, chain.no_chain), (PathBuf::from("exams"), None, false));
        assert!(Cli::try_parse_from(["ignot", "verify", "x", "--no-chain", "--rpc", "https://node"]).is_err());
    }
}
