# Transform Pipeline

This document describes the image transformation pipeline that `transform_raster()` applies to raster inputs.

## Pipeline stages

```text
decode → auto-orient → rotate → crop → resize → blur → sharpen → grayscale → watermark → encode
```

| # | Stage | Guard | Description |
|---|-------|-------|-------------|
| 1 | **Decode** | — | Parse input bytes into a `DynamicImage` using the detected codec (JPEG, PNG, WebP, AVIF, BMP, TIFF, GIF). |
| 2 | **Auto-orient** | `auto_orient == true` | Apply EXIF orientation tag (JPEG only, tags 2–8). |
| 3 | **Rotate** | `rotate != 0` | Explicit rotation by 0°, 90°, 180°, or 270°. |
| 4 | **Crop** | `crop` set | Extract a sub-region defined by `(x, y, width, height)`. |
| 5 | **Resize** | `width` and/or `height` set | Scale the image according to `fit` (contain / cover / fill / inside) and `position`. |
| 6 | **Blur** | `blur` set | Gaussian blur with the given sigma (0.1–100.0). |
| 7 | **Sharpen** | `sharpen` set | Unsharp mask with the given sigma (0.1–100.0). |
| 8 | **Grayscale** | `grayscale == true` | Collapse the color channels to luminance (Rec. 601 weights), preserving alpha. Runs before the watermark so an overlay keeps its own colors. |
| 9 | **Watermark** | `watermark` provided | Alpha-composite a watermark image at the specified position, opacity, and margin. |
| 10 | **Encode** | — | Encode to the output format (JPEG, PNG, WebP, AVIF, BMP, TIFF) with optional quality and metadata injection. GIF is not an output format. |

Each stage checks the optional deadline (server: 30 s) and returns `TransformError::LimitExceeded` if exceeded.

## Deadline checkpoints

The server adapter injects a 30-second deadline. The pipeline checks elapsed time after decode, rotate, crop, resize, blur, sharpen, grayscale, watermark, and encode. The CLI does not set a deadline.

## GIF input

GIF decodes through this pipeline like any other raster format, with two constraints that
sit ahead of it in `codecs::transform`:

- `format: gif` is rejected with `UnsupportedOutputMediaType`. truss has no GIF encoder;
  palette quantization and frame disposal are a different problem from the single-frame
  pipeline.
- An input whose `frame_count` is greater than 1 is rejected with
  `UnsupportedInputMediaType`, naming the frame count. Reducing an animation to its first
  frame and reporting success would discard data with no signal to the caller. `sniff_artifact`
  still reports `frame_count` for animated GIFs, so `inspect` works on the same file.

Because "keep the input format" cannot resolve to GIF, `TransformOptions::normalize` resolves
an absent `format` to PNG for GIF input: lossless, and able to reproduce a palette and a
transparent color index exactly.

## SVG path

SVG inputs are handled by `transform_svg()`, not by this pipeline. If `crop`, `blur`, `sharpen`, or `watermark` is requested for an SVG input, the request is rejected with `InvalidOptions`.

`grayscale` is accepted for SVG input when the output is a raster format: it is applied to the rasterized pixels, after rotation, in the same position it occupies in the raster pipeline. SVG-to-SVG output ignores it, along with the other raster-only options.
