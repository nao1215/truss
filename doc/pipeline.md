# Transform Pipeline

This document describes the image transformation pipeline that `transform_raster()` applies to raster inputs.

## Pipeline stages

```text
decode → auto-orient → rotate → resize → blur → watermark → encode
```

| # | Stage | Guard | Description |
|---|-------|-------|-------------|
| 1 | **Decode** | — | Parse input bytes into a `DynamicImage` using the detected codec (JPEG, PNG, WebP, AVIF, BMP). |
| 2 | **Auto-orient** | `auto_orient == true` | Apply EXIF orientation tag (JPEG only, tags 2–8). |
| 3 | **Rotate** | `rotate != 0` | Explicit rotation by 0°, 90°, 180°, or 270°. |
| 4 | **Resize** | `width` and/or `height` set | Scale the image according to `fit` (contain / cover / fill / inside) and `position`. |
| 5 | **Blur** | `blur` set | Gaussian blur with the given sigma (0.1–100.0). |
| 6 | **Watermark** | `watermark` provided | Alpha-composite a watermark image at the specified position, opacity, and margin. |
| 7 | **Encode** | — | Encode to the output format (JPEG, PNG, WebP, AVIF, BMP) with optional quality and metadata injection. |

Each stage checks the optional deadline (server: 30 s) and returns `TransformError::LimitExceeded` if exceeded.

## Deadline checkpoints

The server adapter injects a 30-second deadline. The pipeline checks elapsed time after decode, rotate, resize, blur, watermark, and encode. The CLI does not set a deadline.

## SVG path

SVG inputs are handled by `transform_svg()`, not by this pipeline. If `blur` or `watermark` is requested for an SVG input, the request is rejected with `InvalidOptions`.
