/*
 * Copyright (C) 2026 Arseniy Astankov
 *
 * This file is part of img_optimizer.
 *
 * img_optimizer is free software: you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation, either version 3 of the License, or (at your option)
 * any later version.
 *
 * img_optimizer is distributed in the hope that it will be useful, but WITHOUT
 * ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
 * FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License along with
 * img_optimizer. If not, see <https://www.gnu.org/licenses/>.
 */

mod optimizer;

use std::{
    fs,
    io::{self, BufRead},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Parser;

use crate::optimizer::{OPTIMIZABLE_EXTS, optimize_images};

#[derive(Debug, clap::Parser)]
#[command(about = "Optimize images by similarity using the x265 codec", version)]
struct Args {
    /// Base directory used for paths recorded in the output metadata.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Directory where optimized.mkv, metadata.csv and optimized_files.txt are written.
    #[arg(short, long, default_value = ".")]
    output_dir: PathBuf,
    /// Read image paths from a file, one per line. Use "-" to read from stdin.
    #[arg(long)]
    files_from: Option<PathBuf>,
    /// Follow symbolic links when walking directories.
    #[arg(short = 'L', long = "dereference")]
    dereference: bool,
    /// x265 CRF value (0 is lossless, higher values mean more compression).
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=51))]
    crf: u8,
    /// Image files or directories to optimize. Directories are scanned recursively.
    inputs: Vec<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let root = args.root.canonicalize().unwrap_or(args.root);
    let output_dir = args.output_dir;

    let mut inputs = Vec::new();
    let mut had_explicit_input = false;

    for input in &args.inputs {
        had_explicit_input = true;
        collect_input(input, args.dereference, &mut inputs)?;
    }

    if let Some(files_from) = &args.files_from {
        had_explicit_input = true;
        read_paths_from(files_from, &mut inputs)?;
    }

    if !had_explicit_input {
        inputs = find_optimizable(&root, args.dereference)?;
    }

    inputs.sort();
    inputs.dedup();

    if inputs.is_empty() {
        anyhow::bail!("no image files found");
    }

    let result = optimize_images(&root, &inputs, &output_dir, args.crf)?;

    eprintln!(
        "optimized {} image(s): {} image data, {} metadata",
        result.files.len(),
        result.video.display(),
        result.metadata.display(),
    );

    Ok(())
}

fn collect_input(path: &Path, dereference: bool, out: &mut Vec<PathBuf>) -> Result<()> {
    let meta = file_metadata(path, dereference)
        .with_context(|| format!("failed to inspect {}", path.display()))?;

    if meta.is_dir() {
        walk_dir(path, dereference, out)?;
    } else if meta.is_file() {
        out.push(path.to_path_buf());
    }

    Ok(())
}

fn find_optimizable(root: &Path, dereference: bool) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    walk_dir(root, dereference, &mut result)?;
    Ok(result)
}

fn walk_dir(dir: &Path, dereference: bool, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let meta = match file_metadata(&path, dereference) {
            Ok(meta) => meta,
            Err(_) => continue,
        };

        if meta.is_dir() {
            let _ = walk_dir(&path, dereference, out);
        } else if meta.is_file() && is_optimizable(&path) {
            out.push(path);
        }
    }

    Ok(())
}

fn file_metadata(path: &Path, dereference: bool) -> io::Result<fs::Metadata> {
    if dereference {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    }
}

fn is_optimizable(path: &Path) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    OPTIMIZABLE_EXTS.contains(&ext.as_str())
}

fn read_paths_from(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let reader: Box<dyn BufRead> = if path == Path::new("-") {
        Box::new(io::stdin().lock())
    } else {
        Box::new(io::BufReader::new(
            fs::File::open(path)
                .with_context(|| format!("failed to open {}", path.display()))?,
        ))
    };

    for line in reader.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            out.push(PathBuf::from(line));
        }
    }

    Ok(())
}
