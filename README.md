# img_optimizer

A command-line tool that optimizes images (jpg, jpeg, png) by grouping similar
images and re-encoding them with the x265 codec. It requires the `ffmpeg` and
`exiftool` utilities to be available.

## Usage

Optimize every image found under a directory:

```sh
img_optimizer --root ./photos --output-dir ./optimized ./photos
```

Optimize an explicit list of images:

```sh
img_optimizer --root ./photos --output-dir ./optimized ./photos/a.jpg ./photos/b.png
```

Choose a lossy x265 CRF value instead of the lossless default (`0`):

```sh
img_optimizer --root ./photos --output-dir ./optimized --crf 18 ./photos
```

Pass a list of files through a file or stdin:

```sh
find ./photos -type f \( -iname '*.jpg' -o -iname '*.png' \) |
    img_optimizer --root ./photos --output-dir ./optimized --files-from -
```

### Output

The tool writes three files into the output directory:

- `optimized.mkv`: the x265 video containing the optimized image sequence.
- `metadata.csv`: metadata for each optimized image.
