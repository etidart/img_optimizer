use std::{
    fs::{self, File},
    path::{Component, Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use base64::{Engine, prelude::BASE64_STANDARD};
use rayon::prelude::*;

static EXIF_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct FrameMetadata {
    filename: String,
    resolution: (u32, u32),
    rotated: bool,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    mtime: Option<u64>,
    exif_metadata: Option<Vec<u8>>,
}

pub struct UnpackerOutput {
    pub files: Vec<PathBuf>,
}

fn parse_u32(value: &str) -> Option<u32> {
    value.parse().ok()
}

fn parse_u64(value: &str) -> Option<u64> {
    value.parse().ok()
}

fn read_metadata(csv_path: &Path) -> Result<impl Iterator<Item = Result<FrameMetadata>>> {
    let file =
        File::open(csv_path).with_context(|| format!("failed to open {}", csv_path.display()))?;
    let mut reader = csv::Reader::from_reader(file);
    let headers = reader.headers()?.clone();

    fn index(headers: &csv::StringRecord, name: &str) -> Result<usize> {
        headers
            .iter()
            .position(|header| header == name)
            .with_context(|| format!("metadata CSV has no '{name}' column"))
    }

    let filename_i = index(&headers, "filename")?;
    let resolution_i = index(&headers, "resolution")?;
    let rotated_i = index(&headers, "rotated").ok();
    let mode_i = index(&headers, "mode").ok();
    let uid_i = index(&headers, "owner_uid").ok();
    let gid_i = index(&headers, "owner_gid").ok();
    let mtime_i = index(&headers, "mtime").ok();
    let exif_i = index(&headers, "exif_metadata").ok();

    Ok(reader
        .into_records()
        .enumerate()
        .map(move |(row_number, record)| {
            let result: Result<FrameMetadata> = (|| {
                let record =
                    record.with_context(|| format!("failed to read CSV row {}", row_number + 2))?;
                let get = |index: usize| record.get(index).unwrap_or_default();

                let filename = get(filename_i).trim().to_string();
                if filename.is_empty() {
                    bail!("CSV row {} has an empty filename", row_number + 2);
                }

                let resolution_text = get(resolution_i);
                let (width, height) = resolution_text
                    .split_once('x')
                    .context("invalid resolution metadata")?;
                let resolution = (
                    width.parse::<u32>().context("invalid resolution width")?,
                    height.parse::<u32>().context("invalid resolution height")?,
                );

                let rotated = rotated_i
                    .and_then(|i| record.get(i))
                    .is_some_and(|value| value == "1");
                let mode = mode_i
                    .and_then(|i| record.get(i))
                    .and_then(|value| u32::from_str_radix(value.trim(), 8).ok());
                let uid = uid_i.and_then(|i| record.get(i)).and_then(parse_u32);
                let gid = gid_i.and_then(|i| record.get(i)).and_then(parse_u32);
                let mtime = mtime_i.and_then(|i| record.get(i)).and_then(parse_u64);
                let exif_metadata = exif_i
                    .and_then(|i| record.get(i))
                    .filter(|value| !value.is_empty())
                    .and_then(|value| BASE64_STANDARD.decode(value).ok());

                Ok(FrameMetadata {
                    filename,
                    resolution,
                    rotated,
                    mode,
                    uid,
                    gid,
                    mtime,
                    exif_metadata,
                })
            })();
            result
        }))
}

fn safe_output_path(output_dir: &Path, filename: &str) -> Result<PathBuf> {
    let path = Path::new(filename);
    if path.is_absolute() {
        bail!("metadata filename must be relative: {filename}");
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("metadata filename contains an unsafe path: {filename}");
    }
    Ok(output_dir.join(path))
}

fn restore_exif(path: &Path, data: &[u8]) -> Result<()> {
    let counter = EXIF_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mie_path = std::env::temp_dir().join(format!(
        "img_optimizer_exif_{}_{}.mie",
        std::process::id(),
        counter
    ));
    fs::write(&mie_path, data)
        .with_context(|| format!("failed to write EXIF temp file {}", mie_path.display()))?;

    let status = Command::new("exiftool")
        .args(["-overwrite_original", "-TagsFromFile"])
        .arg(&mie_path)
        .arg("-all:all")
        .arg(path)
        .status();

    let _ = fs::remove_file(&mie_path);
    if status.is_err() {
        bail!("failed to run exiftool");
    }
    Ok(())
}

fn restore_mode_owner_mtime(path: &Path, item: &FrameMetadata) {
    #[cfg(unix)]
    {
        if let Some(mode) = item.mode {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
        }

        if let (Some(uid), Some(gid)) = (item.uid, item.gid) {
            let _ = Command::new("chown")
                .arg(format!("{uid}:{gid}"))
                .arg(path)
                .status();
        }
    }

    if let Some(mtime) = item.mtime {
        let modified = std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(mtime));
        if let (Ok(file), Some(modified)) = (fs::File::options().write(true).open(path), modified) {
            let times = fs::FileTimes::new().set_modified(modified);
            let _ = file.set_times(times);
        }
    }
}

pub fn unpack_images(
    csv_path: &Path,
    video_path: &Path,
    output_dir: &Path,
) -> Result<UnpackerOutput> {
    let items = read_metadata(csv_path)
        .context("failed to read metadata")?
        .collect::<Result<Vec<_>>>()?;
    if items.is_empty() {
        bail!("metadata CSV contains no images");
    }

    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    let files = items
        .par_iter()
        .enumerate()
        .map(|(index, item)| -> Result<PathBuf> {
            let output_path = safe_output_path(output_dir, &item.filename)?;
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            let _ = fs::remove_file(&output_path);

            let mut filter = format!(
                "select=eq(n\\,{index}),crop={}:{}:0:0",
                item.resolution.0, item.resolution.1
            );
            if item.rotated {
                filter.push_str(",transpose=2");
            }

            let status = Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-i"])
                .arg(video_path)
                .args(["-map", "0:v:0", "-vf"])
                .arg(filter)
                .args(["-fps_mode", "passthrough", "-frames:v", "1"])
                .arg(&output_path)
                .status()
                .context("failed to run ffmpeg")?;
            if !status.success() {
                let _ = fs::remove_file(&output_path);
                bail!("ffmpeg exited with status {status}");
            }
            if !output_path.is_file() {
                let _ = fs::remove_file(&output_path);
                bail!(
                    "frame count mismatch: video has fewer than {} frame(s)",
                    index + 1
                );
            }

            if let Some(exif) = &item.exif_metadata {
                let _ = restore_exif(&output_path, exif);
            }
            restore_mode_owner_mtime(&output_path, item);
            Ok(output_path)
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(UnpackerOutput { files })
}
