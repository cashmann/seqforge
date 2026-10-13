use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use sf_seq::Game;
use walkdir::WalkDir;

#[derive(Parser)]
#[command(name = "seqforge", version, about = "SeqForge N64 music tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Disassemble a sequence (.zseq/.seq/.aseq) to text.
    Disasm {
        input: PathBuf,
        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Sequence dialect.
        #[arg(long, default_value = "oot", value_parser = parse_game)]
        game: Game,
    },
    /// Assemble text back into a binary sequence.
    Asm {
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Override the source's `.game` directive.
        #[arg(long, value_parser = parse_game)]
        game: Option<Game>,
    },
    /// Check that disassemble → assemble reproduces every sequence byte for byte.
    /// Accepts files and directories; looks inside .mmrs/.ootrs packages.
    Roundtrip {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Dialect for loose sequence files (.mmrs → mm and .ootrs → oot are automatic).
        #[arg(long, default_value = "oot", value_parser = parse_game)]
        game: Game,
        /// Print a line per sequence.
        #[arg(short, long)]
        verbose: bool,
    },
}

fn parse_game(s: &str) -> Result<Game, String> {
    Game::from_name(s).ok_or_else(|| format!("unknown game `{s}` (expected oot or mm)"))
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Disasm {
            input,
            output,
            game,
        } => {
            let data = fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
            let dis = sf_seq::disassemble(game, &data);
            match output {
                Some(path) => fs::write(&path, &dis.text)
                    .with_context(|| format!("writing {}", path.display()))?,
                None => print!("{}", dis.text),
            }
            let s = &dis.stats;
            eprintln!(
                "{} bytes: {} code, {} envelope/table, {} raw ({} abandoned paths)",
                s.total, s.code_bytes, s.data_bytes, s.raw_bytes, s.conflicts
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Asm {
            input,
            output,
            game,
        } => {
            let src = fs::read_to_string(&input)
                .with_context(|| format!("reading {}", input.display()))?;
            let bytes =
                sf_seq::assemble(&src, game).with_context(|| input.display().to_string())?;
            fs::write(&output, &bytes).with_context(|| format!("writing {}", output.display()))?;
            eprintln!("wrote {} bytes to {}", bytes.len(), output.display());
            Ok(ExitCode::SUCCESS)
        }
        Command::Roundtrip {
            paths,
            game,
            verbose,
        } => roundtrip(&paths, game, verbose),
    }
}

struct Sample {
    name: String,
    game: Game,
    data: Vec<u8>,
}

const SEQ_EXTS: &[&str] = &["zseq", "seq", "aseq"];

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn collect(paths: &[PathBuf], default_game: Game) -> Result<(Vec<Sample>, Vec<String>)> {
    let mut samples = Vec::new();
    let mut warnings = Vec::new();
    for root in paths {
        if !root.exists() {
            bail!("{} does not exist", root.display());
        }
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let ext = extension(path);
            let package_game = match ext.as_str() {
                "mmrs" => Some(Game::Mm),
                "ootrs" => Some(Game::Oot),
                _ => None,
            };
            if let Some(game) = package_game {
                if let Err(e) = read_package(path, game, &mut samples) {
                    warnings.push(format!("{}: {e:#}", path.display()));
                }
            } else if SEQ_EXTS.contains(&ext.as_str()) {
                let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
                samples.push(Sample {
                    name: path.display().to_string(),
                    game: default_game,
                    data,
                });
            }
        }
    }
    Ok((samples, warnings))
}

fn read_package(path: &Path, game: Game, samples: &mut Vec<Sample>) -> Result<()> {
    let file = fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file).context("not a valid zip")?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let inner = Path::new(entry.name()).to_path_buf();
        if !entry.is_file() || !SEQ_EXTS.contains(&extension(&inner).as_str()) {
            continue;
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        samples.push(Sample {
            name: format!("{}!{}", path.display(), inner.display()),
            game,
            data,
        });
    }
    Ok(())
}

fn roundtrip(paths: &[PathBuf], default_game: Game, verbose: bool) -> Result<ExitCode> {
    let (samples, warnings) = collect(paths, default_game)?;
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    if samples.is_empty() {
        bail!("no sequences found");
    }

    let mut seen = HashSet::new();
    let (mut unique, mut failures) = (0usize, Vec::new());
    let (mut total_bytes, mut code_bytes, mut data_bytes) = (0usize, 0usize, 0usize);

    for s in &samples {
        if !seen.insert((s.game, s.data.clone())) {
            continue;
        }
        unique += 1;
        let dis = sf_seq::disassemble(s.game, &s.data);
        let result = sf_seq::assemble(&dis.text, None);
        let st = &dis.stats;
        total_bytes += st.total;
        code_bytes += st.code_bytes;
        data_bytes += st.data_bytes;

        let outcome = match &result {
            Ok(bytes) if *bytes == s.data => None,
            Ok(bytes) => Some(first_difference(&s.data, bytes)),
            Err(e) => Some(format!("assembler error: {e}")),
        };
        if verbose || outcome.is_some() {
            let pct = if st.total > 0 {
                100.0 * st.code_bytes as f64 / st.total as f64
            } else {
                0.0
            };
            let status = if outcome.is_none() { "ok  " } else { "FAIL" };
            println!("{status} [{}] {:5.1}% code  {}", s.game, pct, s.name);
            if let Some(msg) = &outcome {
                println!("     {msg}");
            }
        }
        if let Some(msg) = outcome {
            failures.push((s.name.clone(), msg));
        }
    }

    println!(
        "\n{} sequences ({} unique): {} passed, {} failed",
        samples.len(),
        unique,
        unique - failures.len(),
        failures.len()
    );
    if total_bytes > 0 {
        println!(
            "decoded {:.1}% as code, {:.1}% as envelopes/tables, {:.1}% left raw",
            100.0 * code_bytes as f64 / total_bytes as f64,
            100.0 * data_bytes as f64 / total_bytes as f64,
            100.0 * (total_bytes - code_bytes - data_bytes) as f64 / total_bytes as f64,
        );
    }
    Ok(if failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn first_difference(expected: &[u8], actual: &[u8]) -> String {
    let at = expected.iter().zip(actual).position(|(a, b)| a != b);
    match at {
        Some(i) => format!(
            "bytes differ at 0x{i:04X} (expected 0x{:02X}, got 0x{:02X})",
            expected[i], actual[i]
        ),
        None => format!(
            "length differs (expected {}, got {})",
            expected.len(),
            actual.len()
        ),
    }
}
