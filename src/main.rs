use std::{
    collections::HashMap,
    error::Error,
    fmt::Display,
    fs::{copy, read_dir, remove_file},
    os::unix::fs::{symlink, MetadataExt},
    path::PathBuf,
};

use clap::Parser;
use colored::Colorize;
use humansize::DECIMAL;
use inquire::{Select, Text};

#[derive(Debug, Parser)]
struct Cli {
    /// Input directory, may either be a directory of directories for an entire season or just a single directory containing subtitle files
    input: PathBuf,
    /// Output directory, must be the path where media files for the respective season/movie is.
    /// If a FILE is used instead, single mode is assumed
    output: PathBuf,
    /// Whether to copy subtitles instead of symlinking them
    #[arg(short, long)]
    copy: bool,
    /// Whether to overwrite existing files
    #[arg(short, long)]
    overwrite: bool,
}

#[derive(Debug, Clone, Copy)]
enum SubtitleSelectionStrategy {
    Alphabetical,
    Size,
    Manual,
}

impl Display for SubtitleSelectionStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Alphabetical => "First alphabetical",
            Self::Size => "Largest",
            Self::Manual => "Manually select",
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Season,
    Single,
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    let mode = if cli.output.is_dir()
        || read_dir(&cli.input)?
            .flatten()
            .all(|i| i.file_type().expect("file type to be readable").is_dir())
    {
        println!("Using {} mode", "season".bold());
        Mode::Season
    } else {
        println!("Using {} mode", "single".bold());
        Mode::Single
    };

    if cli.overwrite {
        println!("{}", "Overwrite mode enabled!".red());
    }

    println!(
        "In {} mode",
        if cli.copy { "copying" } else { "symlinking" }.bold()
    );

    println!("Reading destination...");

    let mut destination_stems: HashMap<String, PathBuf> = if cli.output.is_dir() {
        read_dir(&cli.output)?
            .flatten()
            .filter(|de| !de.path().is_dir() && !de.path().extension().is_some_and(|e| e == "srt"))
            .map(|i| {
                (
                    i.path()
                        .file_stem()
                        .unwrap_or(&i.file_name())
                        .to_string_lossy()
                        .to_string(),
                    i.path(),
                )
            })
            .collect()
    } else {
        [(
            cli.output
                .file_stem()
                .unwrap_or(cli.output.file_name().expect("file name"))
                .to_string_lossy()
                .to_string(),
            cli.output.clone(),
        )]
        .into_iter()
        .collect()
    };

    if destination_stems.is_empty() {
        eprintln!("{}", "No destination files!".red().bold());
        return Err("No files".into());
    }

    let num_stems = destination_stems.len();

    println!(
        "Destination read with {} {}",
        num_stems.to_string().bold(),
        if destination_stems.len() > 1 {
            "entires"
        } else {
            "entry"
        }
    );

    let strategy = Select::new(
        "Select a strategy:",
        vec![
            SubtitleSelectionStrategy::Alphabetical,
            SubtitleSelectionStrategy::Size,
            SubtitleSelectionStrategy::Manual,
        ],
    )
    .prompt()?;

    let sort_strat = if matches!(strategy, SubtitleSelectionStrategy::Manual) {
        match Select::new("Select a display sort type: ", vec!["Name", "Size"]).prompt()? {
            "Name" => SubtitleSelectionStrategy::Alphabetical,
            "Size" => SubtitleSelectionStrategy::Size,
            _ => unreachable!(),
        }
    } else {
        strategy
    };

    let required_text = Text::new("Enter subtitle file name keyword (optional):").prompt()?;
    let required_text = if required_text.is_empty() {
        None
    } else {
        Some(required_text.to_lowercase())
    };

    match mode {
        Mode::Season => {
            // Match the subs folder to the media name
            let mut entries: Vec<_> = read_dir(&cli.input)?.flatten().collect();
            entries.sort_unstable_by_key(|e| e.file_name());
            for sub_dir in entries {
                let dir_name = sub_dir.file_name().to_string_lossy().to_string();
                if let Some(media_file) = destination_stems.remove(&dir_name) {
                    synchronize_folder(
                        &sub_dir.path(),
                        &media_file,
                        strategy,
                        sort_strat,
                        cli.copy,
                        cli.overwrite,
                        &required_text,
                    )?;
                }
            }
        }
        Mode::Single => {
            {
                let media_file = destination_stems.iter().next().expect("one item exactly").1;
                synchronize_folder(
                    &cli.input,
                    media_file,
                    strategy,
                    sort_strat,
                    cli.copy,
                    cli.overwrite,
                    &required_text,
                )?;
            }
            destination_stems.clear();
        }
    }

    if destination_stems.is_empty() {
        println!("{}", "Done!".green().bold());
    } else {
        println!(
            "{}",
            format!(
                "Completed with {} matches, but didn't match:",
                num_stems - destination_stems.len()
            )
            .yellow()
            .bold()
        );
        for stem in destination_stems.keys() {
            println!(" - {}", stem);
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ManualSelectionData {
    name: String,
    size: u64,
}

impl Display for ManualSelectionData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!(
            "{} ({})",
            self.name,
            humansize::format_size(self.size, DECIMAL)
        ))
    }
}

fn synchronize_folder(
    sub_dir: &PathBuf,
    dest_file: &PathBuf,
    strategy: SubtitleSelectionStrategy,
    sort_strat: SubtitleSelectionStrategy,
    copy_sub: bool,
    overwrite: bool,
    required_text: &Option<String>,
) -> Result<(), Box<dyn Error>> {
    let mut subtitle_files: Vec<_> = read_dir(sub_dir)?
        .flatten()
        .filter(|de| {
            de.path().extension().map(|e| e == "srt").unwrap_or(false)
                && (required_text.is_none()
                    || required_text.as_ref().is_some_and(|rt| {
                        de.file_name().to_string_lossy().to_lowercase().contains(rt)
                    }))
        })
        .collect();

    if subtitle_files.is_empty() {
        eprintln!(
            "{} {}",
            "No subtitles in sub directory ".red().bold(),
            sub_dir
                .file_name()
                .expect("filename")
                .to_string_lossy()
                .red()
                .bold()
        );
        return Err("Empty subtitle dir".into());
    }

    match sort_strat {
        SubtitleSelectionStrategy::Alphabetical => {
            subtitle_files.sort_unstable_by_key(|entry| entry.file_name());
        }
        SubtitleSelectionStrategy::Size => {
            subtitle_files
                .sort_unstable_by_key(|entry| entry.metadata().expect("file metadata").size());
        }
        SubtitleSelectionStrategy::Manual => unreachable!(),
    }

    let source_sub = match strategy {
        SubtitleSelectionStrategy::Alphabetical => {
            subtitle_files.first().expect("must be at least one entry")
        }
        SubtitleSelectionStrategy::Size => {
            subtitle_files.last().expect("must be at least one entry")
        }
        SubtitleSelectionStrategy::Manual => {
            let choices = subtitle_files
                .iter()
                .into_iter()
                .map(|de| ManualSelectionData {
                    name: de.file_name().to_string_lossy().to_string(),
                    size: de.metadata().expect("file metadata").size(),
                })
                .collect();

            let choice = Select::new(
                &format!(
                    "Select a subtitle file for {}:",
                    dest_file
                        .file_name()
                        .expect("file name")
                        .to_string_lossy()
                        .bold()
                ),
                choices,
            )
            .prompt()?;

            subtitle_files
                .iter()
                .find(|de| de.file_name().to_string_lossy() == choice.name)
                .expect("must be a selected choice")
        }
    };

    let target_name = dest_file
        .parent()
        .expect("dest file to have parent")
        .join(format!(
            "{}.srt",
            &dest_file
                .file_stem()
                .expect("dest file stem")
                .to_string_lossy()
        ));

    if target_name.exists() && overwrite {
        println!(
            "{}",
            format!("Replacing file {}", target_name.to_string_lossy()).red()
        );
        remove_file(&target_name)?;
    }

    if copy_sub {
        copy(source_sub.path(), target_name)?;
    } else {
        symlink(source_sub.path(), target_name)?;
    }

    Ok(())
}
