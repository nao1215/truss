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
| 3 | **Rotate** | `rotate != 0` | Clockwise rotation by any whole number of degrees. A multiple of 90 permutes pixels exactly; any other angle resamples bilinearly, grows the canvas to the rotated bounding box, and fills the exposed corners with `background`. |
| 4 | **Crop** | `crop` set | Extract a sub-region defined by `(x, y, width, height)`. |
| 5 | **Resize** | `width` and/or `height` set | Scale the image according to `fit` and `position`, honouring `without_enlargement`. See [Resize](#resize). |
| 6 | **Blur** | `blur` set | Gaussian blur with the given sigma (0.1–100.0). |
| 7 | **Sharpen** | `sharpen` set | Unsharp mask with the given sigma (0.1–100.0). |
| 8 | **Grayscale** | `grayscale == true` | Collapse the color channels to luminance (Rec. 601 weights), preserving alpha. Runs before the watermark so an overlay keeps its own colors, and after the stages that fill with `background`, so rotation corners and `fit=contain` padding are desaturated along with the image. |
| 9 | **Watermark** | `watermark` provided | Alpha-composite a watermark image at the specified position, opacity, and margin. |
| 10 | **Encode** | — | Encode to the output format (JPEG, PNG, WebP, AVIF, BMP, TIFF) with optional quality and metadata injection. GIF is not an output format. |

Each stage checks the optional deadline (server: 30 s) and returns `TransformError::LimitExceeded` if exceeded.

## Deadline checkpoints

The server adapter injects a 30-second deadline. The pipeline checks elapsed time after decode, rotate, crop, resize, blur, sharpen, grayscale, watermark, and encode. The CLI does not set a deadline.

## Resize

`fit` decides how the image is arranged relative to the requested box. All four modes scale
with the same Lanczos3 filter; they differ in what the output size ends up being.

| Mode | Scale factor | Output size |
|---|---|---|
| `contain` | `min(tw/w, th/h)` | exactly the box, padded with `background` |
| `inside` | `min(tw/w, th/h)` | the scaled content, no padding |
| `cover` | `max(tw/w, th/h)` | exactly the box, cropped at `position` |
| `fill` | per axis | exactly the box |

`contain` and `inside` compute the same scale. The only difference is that `contain` pads the
result out to the box and `inside` returns it, which is why a 640x427 source bounded by
200x200 is 200x200 under `contain` and 200x133 under `inside`.

`without_enlargement` is deliberately not part of any fit mode. It clamps the scale factor at
`1.0` (and, for `fill`, clamps each target axis to the source), which is meaningful for every
mode and for a single-axis resize. `contain` still reports the full box, because padding out
to the requested size is what `contain` means; only the content inside stops growing.

`resolved_output_dimensions` is the single place these rules live. Both the resize itself and
the `MAX_OUTPUT_PIXELS` check read from it, so the limit is always applied to the size that
actually gets allocated.

## Rotation

`rotate` is normalized into `0..360` before it reaches the pipeline, so a negative angle
turns counter-clockwise and an angle past a full turn wraps. Degrees are whole numbers: the
value goes verbatim into the cache key and the signed-URL canonical string, and a fractional
angle would have to survive Rust and JavaScript float formatting identically for a signature
to verify.

A multiple of 90 takes an exact path that only permutes pixels. Any other angle:

- maps each destination pixel back through the inverse rotation and samples the source
  bilinearly, in premultiplied alpha so a transparent neighbor does not bleed its color into
  the rotated edge;
- expands the canvas to the axis-aligned bounding box of the rotated image, so the corners
  are never cropped away;
- fills the exposed area with `background`, defaulting to transparent, or white for output
  formats that carry no alpha channel — the same rule `fit=contain` padding already uses;
- is checked against `MAX_OUTPUT_PIXELS` before the canvas is allocated, because the input
  budget is larger than the output one and a 45-degree turn nearly doubles the pixel count.

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
