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

use std::{
    fs::{self, File},
    io::BufWriter,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    thread,
};

use anyhow::{Context, Result};
use base64::{Engine, prelude::BASE64_STANDARD};
use image::{
    EncodableLayout, GenericImage, ImageBuffer, ImageEncoder,
    codecs::png::PngEncoder,
    imageops::{FilterType, grayscale, resize, rotate90},
};
use kdtree::{KdTree, distance::squared_euclidean};
use rand::distr::{Alphanumeric, SampleString};

pub const OPTIMIZABLE_EXTS: [&str; 3] = ["jpeg", "jpg", "png"];

const FEATURE_DIM: u32 = 32;
const FEATURE_LEN: usize = (FEATURE_DIM * FEATURE_DIM) as usize;

pub struct OptimizerOutput {
    pub video: PathBuf,
    pub metadata: PathBuf,
    pub files: Vec<PathBuf>,
}

struct ImageData {
    path: PathBuf,
    res: (u32, u32),
    rotate: bool,
}

fn get_feature_vec_and_data(path: PathBuf) -> Result<(Vec<f64>, ImageData)> {
    let img = image::open(&path).context("failed to open and read image")?.to_rgb8();
    let rotate = img.width() < img.height();
    let img = if rotate { rotate90(&img) } else { img };
    let res = (img.width(), img.height());
    let gray = grayscale(&img);
    let small = resize(&gray, FEATURE_DIM, FEATURE_DIM, FilterType::Lanczos3);
    let mut vec = Vec::with_capacity(FEATURE_LEN);
    for y in 0..FEATURE_DIM {
        for x in 0..FEATURE_DIM {
            let pixel = small.get_pixel(x, y).0[0];
            vec.push(pixel as f64 / 255.0);
        }
    }
    Ok((vec, ImageData { path, res, rotate }))
}

fn retrieve_data_and_sort(paths: Vec<PathBuf>) -> Vec<ImageData> {
    let mut feature_vecs = Vec::with_capacity(paths.len());
    let mut paths_with_data = Vec::with_capacity(paths.len());
    for path in paths.into_iter() {
        if let Ok(res) = get_feature_vec_and_data(path) {
            feature_vecs.push(res.0);
            paths_with_data.push(Some(res.1));
        }
    }
    let n = feature_vecs.len();
    if n == 0 {
        return Vec::new();
    }

    let mut kdtree = KdTree::new(FEATURE_LEN);
    for (i, feat) in feature_vecs.iter().enumerate() {
        let arr: [f64; FEATURE_LEN] = feat.clone().try_into().unwrap();
        kdtree.add(arr, i).unwrap();
    }

    let mut visited = vec![false; n];
    let mut sequence = Vec::with_capacity(n);

    let mut current = 0usize;
    visited[current] = true;
    sequence.push(current);

    for _ in 1..n {
        let last_feat: [f64; FEATURE_LEN] = feature_vecs[current].clone().try_into().unwrap();
        let neighbors = kdtree.nearest(&last_feat, 20, &squared_euclidean).unwrap();
        let mut next = None;
        for (_, idx) in neighbors {
            if !visited[*idx] {
                next = Some(*idx);
                break;
            }
        }

        let next = next.unwrap_or_else(|| {
            (0..n).find(|&i| !visited[i]).unwrap()
        });
        visited[next] = true;
        sequence.push(next);
        current = next;
    }

    assert_eq!(sequence.len(), paths_with_data.len());

    let mut sorted_paths_with_data = Vec::with_capacity(sequence.len());
    for i in sequence {
        sorted_paths_with_data.push(paths_with_data[i].take().unwrap());
    }

    sorted_paths_with_data
}

fn get_random_filepath() -> PathBuf {
    let mut rand_str = Alphanumeric.sample_string(&mut rand::rng(), 16);
    let mut path = std::env::temp_dir().join(rand_str + ".mie");
    while path.exists() {
        rand_str = Alphanumeric.sample_string(&mut rand::rng(), 16);
        path = std::env::temp_dir().join(rand_str + ".mie");
    }
    path
}

fn retrieve_exif_data(path: &Path) -> String {
    let tmp_path = get_random_filepath();
    let status = Command::new("exiftool")
        .arg("-TagsFromFile").arg(path)
        .arg("-all:all").arg(&tmp_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let res = match status {
        Ok(s) if s.success() => {
            match fs::read(&tmp_path) {
                Ok(data) => BASE64_STANDARD.encode(data),
                _ => String::new()
            }
        }
        _ => String::new()
    };
    let _ = fs::remove_file(tmp_path);
    res
}

fn collect_metadata_and_write(items: &[ImageData], output: &Path, root: &Path) -> Result<()> {
    let file = File::create(output)
        .with_context(|| format!("failed to create {}", output.display()))?;
    let mut wrt = csv::Writer::from_writer(file);

    wrt.write_record(&[
        "filename",
        "resolution",
        "rotated",
        "mode",
        "owner_uid",
        "owner_gid",
        "mtime",
        "exif_metadata",
    ])?;

    for data in items {
        let meta = fs::metadata(&data.path).ok();
        let mode = meta
            .as_ref()
            .and_then(|m| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    Some(m.permissions().mode())
                }
                #[cfg(not(unix))]
                {
                    None
                }
            })
            .map(|perm: u32| format!("{:o}", perm))
            .unwrap_or_default();
        let uid = meta
            .as_ref()
            .and_then(|m| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    Some(m.uid())
                }
                #[cfg(not(unix))]
                {
                    None
                }
            })
            .map(|id: u32| id.to_string())
            .unwrap_or_default();
        let gid = meta
            .as_ref()
            .and_then(|m| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    Some(m.gid())
                }
                #[cfg(not(unix))]
                {
                    None
                }
            })
            .map(|id: u32| id.to_string())
            .unwrap_or_default();
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default();
        let exif_b64 = retrieve_exif_data(&data.path);

        wrt.write_record(&[
            data.path.strip_prefix(root).unwrap_or(&data.path).to_string_lossy().to_string(),
            format!("{}x{}", data.res.0, data.res.1),
            if data.rotate { "1" } else { "" }.to_string(),
            mode,
            uid,
            gid,
            mtime,
            exif_b64,
        ])?;
    }

    wrt.flush()?;
    Ok(())
}

fn launch_ffmpeg(paths_with_data: Arc<Vec<ImageData>>, video_path: &Path) -> Result<thread::JoinHandle<Result<()>>> {
    let mut max_w = 0u32;
    let mut max_h = 0u32;
    for data in &*paths_with_data {
        max_w = max_w.max(data.res.0);
        max_h = max_h.max(data.res.1);
    }

    let output = File::create(video_path)
        .with_context(|| format!("failed to create {}", video_path.display()))?;

    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-y", "-f", "image2pipe", "-vcodec", "png", "-i", "-",
            "-c:v", "libx265", "-crf", "0",
            "-preset", "veryfast", "-pix_fmt", "yuv444p",
            "-color_range", "full", "-f", "matroska", "pipe:1"
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::from(output))
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn ffmpeg")?;

    let stdin = ffmpeg.stdin.take().unwrap();

    let handle = thread::spawn(move || -> Result<()> {
        let mut ffmpeg = ffmpeg;
        let mut canvas = ImageBuffer::new(max_w, max_h);
        let mut stdin = BufWriter::new(stdin);
        for data in &*paths_with_data {
            if let Ok(img) = image::open(&data.path) {
                let img = img.to_rgb8();
                let img = if data.rotate { rotate90(&img) } else { img };

                canvas.fill(0);
                let _ = canvas.copy_from(&img, 0, 0);

                let encoder = PngEncoder::new(&mut stdin);
                let _ = encoder.write_image(
                    canvas.as_bytes(),
                    canvas.width(),
                    canvas.height(),
                    image::ExtendedColorType::Rgb8,
                );
            }
        }
        drop(stdin);

        let status = ffmpeg.wait().context("failed to wait for ffmpeg")?;
        if !status.success() {
            anyhow::bail!("ffmpeg exited with status {status}");
        }
        Ok(())
    });

    Ok(handle)
}

pub fn optimize_images(root: &Path, image_files: &[PathBuf], output_dir: &Path) -> Result<OptimizerOutput> {
    if image_files.is_empty() {
        anyhow::bail!("no image files found");
    }

    let image_files = retrieve_data_and_sort(image_files.to_vec());
    if image_files.is_empty() {
        anyhow::bail!("no valid image files found");
    }

    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let video = output_dir.join("optimized.mkv");
    let metadata = output_dir.join("metadata.csv");

    let files: Vec<PathBuf> = image_files.iter().map(|data| data.path.clone()).collect();

    let image_files = Arc::new(image_files);
    let metadata_items = Arc::clone(&image_files);
    let metadata_path = metadata.clone();
    let root_owned = root.to_path_buf();

    let metadata_handle = thread::spawn(move || {
        collect_metadata_and_write(&metadata_items, &metadata_path, &root_owned)
    });

    let ffmpeg_handle = launch_ffmpeg(image_files, &video)?;

    metadata_handle
        .join()
        .map_err(|_| anyhow::anyhow!("metadata thread panicked"))??;
    ffmpeg_handle
        .join()
        .map_err(|_| anyhow::anyhow!("ffmpeg thread panicked"))??;

    Ok(OptimizerOutput { video, metadata, files })
}
