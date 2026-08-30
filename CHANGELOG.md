# Changelog

## [Unreleased]

### Fixed

- A trickling client can no longer hold a server worker open indefinitely. The socket read timeout is an inactivity timeout and resets on every byte, and a worker thread is tied to a connection from accept to close, so a client sending one header line every twenty seconds held its worker forever. With the default pool of `max(TRUSS_MAX_CONCURRENT_TRANSFORMS, 8)` threads, sixty-four such connections — a few bytes a minute, no authentication, no body, any endpoint — took the whole server down and kept it down, `/health/live` included, which also means an orchestrator sees the process as dead and restarts it into the same state. The header phase now has a wall-clock budget of fifteen seconds that a trickle cannot extend, and a connection that runs out of it gets `408 Request Timeout` and is closed. A read that times out while the headers are incomplete also answers `408` rather than `500`, which said the server had failed when the client had gone quiet. Reading a request body keeps the longer inactivity timeout: an upload can legitimately be slow and large, and the routes that accept one reject an unauthenticated request from its headers alone, before any body is read. Note that enough simultaneous connections can still occupy the pool for the length of the budget; not tying a worker to a connection during I/O is a larger change and is not part of this fix.
- `truss optimize` no longer returns more bytes than it was given ([#333](https://github.com/nao1215/truss/issues/333), [#334](https://github.com/nao1215/truss/issues/334)). An encoder does not always beat whatever produced the input, and `optimize` had no way to say so: `auto` compared two re-encodes and never the bytes it was handed, so a flat-colour JPEG came back up to 1.65x larger than it started — having paid a generation loss for the extra bytes — while `--mode lossless` on the same file returned it untouched. `auto` is the default mode, so that is what a caller got when they did not choose. An indexed-colour PNG had a second reason to grow: there is no encoder for colour type 3 here, so PNG-8 was decoded to truecolour and re-encoded 45% larger, which is the opposite of what the command is for, on exactly the files a design tool exports for icons and flat illustrations. Both modes now treat the input's own bytes as a candidate whenever no pixel transform was requested and the metadata policy is already satisfied by the file as it stands, and return whichever is smaller. A JPEG's candidate goes through the existing lossless Huffman pass so the metadata policy still applies; a PNG is handed back only when it carries no chunk the policy would remove, so nothing rewrites the container. Where the encoder does win it still wins: a gradient JPEG and a truecolour PNG optimize exactly as before. `integration/fixtures` gains `indexed.png` and `flat.jpg`, the two cases no existing fixture covered.
- Every distinct `Accept` header no longer writes its own copy of the same cached image ([#329](https://github.com/nao1215/truss/issues/329)). When the output format came from content negotiation, the disk cache key included the raw header string as well as the format it resolved to. The format alone determines the bytes, so `image/webp`, `image/webp;q=1.0`, `image/webp,image/png;q=0.5`, and every other equivalent spelling each got its own entry, each preceded by a full transform. There are unboundedly many such strings and they come straight off the request, so one public signed URL was enough to write as many identical entries as an attacker wanted, and `TRUSS_CACHE_MAX_BYTES` defaults to `0`, which means no eviction reclaims them. No attacker is needed to feel it either: browsers and crawlers send different `Accept` strings for the same intent, so the hit rate was divided by the number of distinct clients. The header is out of the key, so a negotiated request and one that named the format explicitly now share an entry, which is correct because the bytes are the same; `Vary: Accept` is still built per response, so a negotiated response still says it varies. Existing entries miss once and are rewritten.
- `truss optimize --mode lossless` no longer refuses a JPEG carrying an EXIF orientation when the tag will survive into the output ([#330](https://github.com/nao1215/truss/issues/330)). A phone photo carries one, and lossless re-optimization is exactly what such a file wants, so this was the common case rather than an edge one. Lossless optimization re-optimizes the Huffman tables without touching the coefficients, and rotating the pixels would need a decode and a re-encode, so it genuinely cannot apply the orientation; what the check missed is that it does not need to, because a retained tag and the stored pixels describe the same picture they described in the input. `--keep-metadata` and `--preserve-exif` are accepted now. With the metadata stripped there is nowhere for the orientation to go and the request is still refused, but the message names the orientation and the way forward instead of blaming "pixel transforms" the caller never asked for.
- Dropping a non-identity EXIF orientation while leaving the pixels as stored now warns ([#331](https://github.com/nao1215/truss/issues/331)). `--no-auto-orient` keeps the pixels as the file stores them and the default `--strip-metadata` removes the tag that says how to display them, so together they rotate the picture by 90, 180, or 270 degrees. Each flag was doing what it says and nothing said what the combination does, so a batch job over a photo library produced a directory of sideways images and exited 0 on every one. It is a warning rather than an error because the combination is legitimate when the caller knows the tag is wrong; what was not defensible was the silence. `--keep-metadata` keeps the tag and warns about nothing, and neither does an input with orientation 1 or no EXIF at all.

- `fit=cover` no longer allocates an unbounded intermediate buffer ([#316](https://github.com/nao1215/truss/issues/316)). Cover scales the source until it covers the requested box on both axes and crops afterwards, so the buffer it materializes is not the size it returns; the output pixel limit was checked against the cropped size, so a request whose target aspect ratio is far from the source's passed the check and then tried to allocate thousands of times the limit. `truss convert large.png --width 3 --height 9999 --fit cover` on a 10000x1 source asked the allocator for three terabytes and aborted the process, which on the HTTP server meant one request took the whole server down with no response and no recovery. The pre-crop buffer is now checked from dimensions alone, before anything is allocated, the way arbitrary rotation has always been checked, so the request is a `LimitExceeded` error (exit 4, or `413` over HTTP) naming both sizes. `contain`, `inside`, and `fill` scale by the smaller ratio and are unaffected; `withoutEnlargement` clamps the scale at 1.0 and so can never trip the new check.
- Two different sources can no longer share one cache entry ([#317](https://github.com/nao1215/truss/issues/317)). The disk cache derived its source identifier by joining the kind, the reference, the version, and the configuration boundaries with newlines, but a separator that can occur inside a field is not a separator: `("a.png\nv1", "x")` and `("a.png", "v1\nx")` serialized to the same string, so the second request was served the first one's bytes from a cache that reported a hit. Object keys containing a newline are legal on a filesystem and on S3, GCS, and Azure Blob alike, and any deployment that derives `version` from user-controlled data could produce the collision without being under attack. Every field is length-prefixed now, so no value can forge a delimiter; the storage reference length-prefixes its scheme, bucket, and key for the same reason, since a key containing a slash had the same shape of problem. Existing cache entries miss once and are rewritten, so expect a one-time drop in hit rate after upgrading.
- JPEG output composites transparency against the background instead of discarding it ([#318](https://github.com/nao1215/truss/issues/318)). JPEG has no alpha channel, and the encoders reached RGB by dropping the channel and keeping the color samples unchanged, so a 50%-opaque red came out fully saturated and a fully transparent pixel came out black. `--background` had no effect on that path, contradicting the README, and the same source produced the correct result whenever the request happened to include a `contain` resize, because padding resolves transparency against the pad canvas on the way through. Both the raster and the SVG codec now flatten against `--background` before encoding to a format that cannot carry alpha, defaulting to white, which is the rule the padding helper already used. PNG, WebP, AVIF, BMP, and TIFF output keep their alpha untouched.
- EXIF orientations 5 and 7 are no longer each other's transform ([#319](https://github.com/nao1215/truss/issues/319)). TIFF/EXIF defines 5 as "mirror horizontal and rotate 270 CW" and 7 as "mirror horizontal and rotate 90 CW"; the two arms had the rotations the other way round, so an image tagged either way came out mirrored along the wrong diagonal. Pillow's `exif_transpose` and `magick -auto-orient` agree with each other and disagreed with truss on exactly those two values. The existing tests could not catch it: orientations 5 through 8 all turn a 4x2 image into a 2x4 one, and 5 and 7 were the two that had only a dimension assertion. They are now one table-driven test over all eight values, checking where a marker pixel lands against coordinates derived from the specification rather than from the implementation.
- The SVG sanitizer removes SMIL animation elements and `<handler>` ([#320](https://github.com/nao1215/truss/issues/320)). `sanitize_svg` documents that it removes scripts, event handlers, and external references, and two constructs defeated both guarantees. SMIL sets attributes at render time through `to`, `values`, `from`, and `by`, none of which the attribute filter inspected, so `<animate attributeName="href" values="javascript:alert(1)"/>` passed through byte for byte and restored exactly what the filter removes; the same mechanism restored an external `<image href>` through `<set>`. `<handler>`, SVG Tiny's script container, was absent from the forbidden-element list, so the element and its body survived intact. `animate`, `set`, `animateTransform`, `animateMotion`, `animateColor`, and `handler` are all removed now: declarative animation is not something a sanitizing image pipeline needs to preserve, and dropping the elements is what makes the attribute rules hold.
- A signed URL verifies under exactly one spelling of its signature ([#321](https://github.com/nao1215/truss/issues/321)). `docs/signed-url-spec.md` fixes the encoding at 64 lowercase hex characters, but `hex::decode` is case-insensitive and accepts any even-length input, so one signed URL verified under all 2^64 case permutations, each of which is a different URL string. This is not a forgery — the secret is still required — but signed URLs exist to be served through a CDN, and a CDN keys its cache on the URL string, so anyone holding one valid signed URL (a public value, by design) could mint unlimited distinct URLs that all pass verification, all miss the edge cache, and all reach the origin for a full transform. The same applies to request deduplication, per-URL rate limiting, and access logs. The signature text is now checked before it is decoded, which also rejects a signature of the wrong length at parse time rather than at the MAC comparison. Both first-party signers emit lowercase, so no first-party client is affected.

### Added

- `truss inspect` reports the EXIF orientation and the dimensions a transform will produce ([#322](https://github.com/nao1215/truss/issues/322)). `inspect` reads the container and `convert` applies the orientation by default, so the two disagree for every rotated photo, and nothing in the output said so or let a caller work it out. An application that records dimensions at upload time and reuses them for every derivative and every `<img width height>` therefore recorded landscape for a portrait phone photo and served portrait, which is wrong intrinsic dimensions and layout shift on what is probably the most common kind of upload such an application receives. The output gains `orientation`, present only when the input carries the tag, and `orientedWidth` and `orientedHeight`, which are always present and equal `width` and `height` whenever the orientation does not transpose the axes — so a caller can read those two unconditionally and be right in every case. `ArtifactMetadata` carries the orientation and answers `oriented_dimensions()`, and the WASM `inspectImageJson` response gains the same three fields. The transform pipeline and `inspect` now read the tag through one function, so what one reports and the other applies cannot drift.
- `truss capabilities` prints what the binary in front of you can do, as JSON: the formats it decodes and encodes, the pixel stages a transform runs and the order it runs them in, the option vocabularies (fit modes, positions, optimize modes), the optional cargo features it was compiled with, and the pixel limits it enforces. A caller driving truss as a subprocess had no way to ask any of this and had to hardcode it, which goes wrong silently: a release build is not one thing, since AVIF, SVG, lossy WebP, and each storage backend are compile-time choices, and asking for an absent one fails only at transform time. The WASM adapter has answered the same question since it shipped, through `getCapabilitiesJson`; this is that question from the command line. `pipeline` is the field with no other source: truss applies its options in a fixed order whatever order they were given in, so a caller composing an operation chain of its own has to split the chain across invocations when its order differs, and until now that order could only be found by experiment. It is pinned by behavioral tests — rotation before the crop, the crop before the resize, the resize before the watermark — so the declaration cannot drift from what the transform does.
- Fixtures and end-to-end coverage for the above: `integration/fixtures` gains `exif-transposed-5.jpg`, `exif-transposed-7.jpg`, and `svg-animate-xss.svg`, and the atago suite gains scenarios pinning the cover buffer limit, the JPEG compositing equivalence between the direct and the padded paths, orientations 5 and 7 against baselines generated by an independent oracle, the sanitizer against the SMIL payload, the oriented dimensions reported by `inspect`, and the lowercase signature `truss sign` emits. The suite goes from 236 scenarios to 264, the last fifteen of which cover `truss capabilities`, checking each claim it makes against what `convert` actually does.

### Breaking Changes

- `ArtifactMetadata` gains a public `orientation` field. Code that constructs the struct with a literal has to add it; `ArtifactMetadata::default()` and `..Default::default()` are unaffected.
- `TransformWarning` gains an `OrientationDropped` variant. The enum is `#[non_exhaustive]`, so a `match` already needs a wildcard arm and nothing has to change; code that enumerated the variants deliberately should add the new one.

## v0.15.0

### Fixed

- `preserveExif=true` on its own is accepted by the HTTP server. It implies "do not strip" on the CLI and in the WASM build, both of which resolve the pair through `resolve_metadata_flags`, whose documented contract is that every adapter produces the same `(strip_metadata, preserve_exif)` answer. The server read the two fields independently and so answered 400 `preserve_exif requires strip_metadata to be false` unless `stripMetadata=false` was sent beside it, which also made a server-side preset that set only `preserveExif` unusable. It goes through the same resolver now, and an explicit `stripMetadata=true` is overridden the way `--strip-metadata --preserve-exif` is on the CLI rather than refused. `@nao1215/truss-url-signer` applies the same resolution instead of throwing — for `{preserveExif: true}` on its own and for the contradictory pair alike — so all three spellings sign the byte-identical URL that `truss sign --preserve-exif` prints, and no adapter is left answering differently from the rest. The OpenAPI description said "Requires `stripMetadata=false`", which described the defect; it now says the flag implies it.
- Two validation messages named Rust struct fields rather than the option the caller typed: `without_enlargement requires width or height` and `preserve_exif requires strip_metadata to be false`. No adapter spells them that way — the CLI has `--without-enlargement`, the HTTP API and the WASM options object have `withoutEnlargement` — and every other message in the same file already used the public spelling.
- `truss convert in.png -o - --format webp` could write nothing and still exit 0. Standard output is a buffered `LineWriter`, and a short payload with no `0x0A` byte in it — which a small WebP or AVIF thumbnail routinely is — sat entirely in that buffer until the runtime flushed it after `main` returned, where nothing observes the error. A full disk, a quota, or a reader that closed the pipe therefore lost the image silently. The CLI now flushes standard output itself and reports a failure as exit code 5 with the reason on stderr; a command that had already failed keeps its own exit code, since the flush error is a consequence of the first failure rather than a second one.
- `truss --version` and `truss help <topic>` exited 5 with nothing on stderr when standard output could not be written, while every other command printed `error: ...` and said why. They now report the failure the same way.
- A configuration fault that `truss validate` reports as exit 1 was reported by `truss serve` as exit 5, so a deploy script that ran one and then the other saw two different codes for one fault. Configuration errors are exit 1 from both; exit 5 stays for what fails after the configuration is accepted, such as a port already in use.
- A storage root that cannot be resolved now names the setting that chose it. `TRUSS_STORAGE_ROOT` was the only setting in `ServerConfig::from_env` whose error was the bare operating-system message, which reads "No such file or directory (os error 2)" on Linux and macOS and "The system cannot find the path specified. (os error 3)" on Windows, and names neither the variable nor the path. It now reports ``TRUSS_STORAGE_ROOT `<path>` cannot be resolved: <reason>``.
- A watermark that does not fit reports both sizes and the margin instead of "watermark image is too large for the output dimensions". Either the image or the margin can be what does not fit, and the old wording blamed the image in both cases, sending readers off to shrink something that was already small enough. It now reads `watermark 16x16 with a 1000px margin does not fit a 64x64 output`. The check itself is one function now rather than the same condition written twice.

### Changed

- An output format truss never encodes is a usage error whichever way it is asked for. `--format gif` was refused by the flag's value parser with exit 1 and the alternatives spelled out, while `-o out.gif` reached the same wall from the other side and surfaced `unsupported output media type: image/gif` with exit 4 from deep in the pipeline. The output extension is now checked at parse time when `--format` is absent, so both spellings give the same message and the same exit code. An explicit `--format` still wins over the extension, so `-o out.gif --format png` writes a PNG.
- `UnsupportedOutputMediaType` says which rule was broken rather than naming the media type. `svg` output is refused for a raster input and accepted for an SVG one, and `gif` is refused for every input; the old message, `unsupported output media type: image/svg+xml`, left the reader to work that out after having seen `truss diagram.svg -o safe.svg` succeed. The CLI, the HTTP server's 415 detail, and the WASM error all read from the same wording now: `svg output requires an svg input; choose a raster output format such as png, jpeg, webp, or avif`.

### Added

- The end-to-end suite runs on Linux, macOS, and Windows rather than on Linux alone. Every scenario was rewritten to be shell-free: `shell: true` means `/bin/sh` on Linux and macOS but `cmd.exe` on Windows, where neither `$FIXTURES_DIR` nor `cat` nor `cmp` exists, so a suite built on it is three different suites and only one of them ever ran. The shared fixtures now reach the specs as `${fixtures}` through a new `e2e/atago/atago.project.yaml`, pipes and redirects are `stdin.file` and `stdout_to`, and the two scenarios that shelled out to `cmp` to prove two files differ compare against committed baselines instead, which pins what the flag produced rather than only that it produced something.
- End-to-end coverage for the surfaces that had none: `e2e/atago/portability.atago.yaml` (case-folded output extensions, paths with spaces and non-ASCII names, missing and non-directory output paths, stdin/stdout binary integrity for a payload with no newline in it, LF line endings on every platform, and repeat-run determinism), `filters.atago.yaml` (`--crop`, `--blur`, `--sharpen`, including the one property of those two filters that holds exactly and needs no baseline: a uniform image comes back unchanged from both), `watermark.atago.yaml`, `completions.atago.yaml` (every supported shell, PowerShell included), `optimize.atago.yaml`, and `validate.atago.yaml`, plus signer scenarios pinning the URL `truss sign` produces against the constant the TypeScript package's own tests assert. The suite goes from 157 scenarios to 236.

## v0.14.0

### Changed

- Breaking: `fit=inside` no longer pads ([#312](https://github.com/nao1215/truss/issues/312)). It now means what the same name means in sharp and imgproxy: scale to fit inside the requested box, preserve the aspect ratio, and add no padding, so the output is at most the requested size and usually smaller on one axis. A 640x427 source bounded by 200x200 is now 200x133 rather than a 200x200 letterbox. `contain` is unchanged and remains the mode that pads out to the exact box; `cover` and `fill` are unchanged.
- Breaking: `inside` no longer implies "never enlarge". That was a second, unrelated policy folded into a fit mode, and it is now `--without-enlargement` on the CLI, `withoutEnlargement` in the HTTP API, the WASM options object, and `@nao1215/truss-url-signer`. It combines with every fit mode and with a single-axis resize, and it requires `width` or `height`. `contain` still reports the full requested box when it is set; only the content inside stops growing. To keep the old `inside` behavior, ask for `--fit contain --without-enlargement`.

### Added

- `TransformOptions::without_enlargement`, and `withoutEnlargement` as a query parameter, a JSON body field, a WASM option, and a signer transform. It participates in the cache key and in the signed-URL canonical string.

## v0.13.0

### Added

- `--rotate` accepts any whole number of degrees, not only quarter turns ([#303](https://github.com/nao1215/truss/issues/303)), across the CLI, the HTTP API, the WASM build, and `@nao1215/truss-url-signer`. Positive turns clockwise and negative counter-clockwise, and angles are normalized into `0`-`359`, so `-90`, `270`, and `630` are the same rotation. A multiple of 90 keeps the exact pixel-permuting path; any other angle resamples bilinearly in premultiplied alpha, grows the output to the rotated bounding box so no corner is cropped, fills the exposed area with `--background` (transparent by default, white for formats without an alpha channel), and is checked against `MAX_OUTPUT_PIXELS` before the canvas is allocated.
- Static GIF input on `convert`, `optimize`, `inspect`, the HTTP server, and the WASM build ([#301](https://github.com/nao1215/truss/issues/301)). GIF is decode-only: `format=gif` is rejected, and a GIF input that names no output format is encoded as PNG rather than echoed back as GIF. An animated GIF is refused with an error naming the frame count instead of being reduced to its first frame, while `inspect` still reads it and reports `isAnimated`. `integration/fixtures` gains `sample.gif`, `transparent.gif`, and `animated.gif`.
- `--grayscale` on `convert` and `sign`, `grayscale` in the HTTP API (query parameter and JSON body), the WASM options object, and the `@nao1215/truss-url-signer` transform set ([#302](https://github.com/nao1215/truss/issues/302)). Luminance uses the Rec. 601 weights and the alpha channel is preserved. The stage runs after resize, blur, and sharpen and before the watermark, so a watermark keeps its own colors, and it participates in the cache key and the signed-URL canonical string. SVG input is supported when the output is a raster format.

### Changed

- Dependency updates consolidated from Dependabot: `uuid` 1.24 -> 1.26, `ureq` 3.3 -> 3.4, `http` 1.4.2 -> 1.5.0, and `puppeteer-core` 25.4 -> 25.9 in the Vite example.
- The Vite example drops `vite-plugin-top-level-await` and raises its build target to browsers with native top-level await, so nothing has to transform it. `vite` stays on the 7.x line: Vite 8 builds with Rolldown, which emits the `.wasm` asset but leaves no reference to it in the bundle, so the page 404s at runtime.
- Breaking: `Rotation` is a newtype over whole degrees instead of a four-variant enum. `Rotation::Deg90` becomes `Rotation::DEG_90`, `Rotation::from_degrees(i32)` normalizes any integer, and `quarter_turns()` and `is_identity()` replace matching on the variants. `as_degrees()` still returns `u16`, so the cache key and the signed-URL canonical string are unchanged for the angles that were already expressible. In `@nao1215/truss-url-signer` the `QuarterTurn` type is replaced by `RotationDegrees`, and the signer normalizes an angle into `0`-`359` before signing, because the signature covers the query string as sent.
- An input truss can read but cannot process now exits 3 (input error) from the CLI instead of 4 (transform error), matching the documented exit-code table. The only case that reaches this in practice is an animated GIF; unreadable bytes already exited 3.

### Fixed

- Two test fixtures did not have the property they were named for, so the end-to-end scenarios built on them asserted nothing. `integration/fixtures/exif-rotated.jpg` carried no EXIF Orientation tag at all, because `magick -set 'EXIF:Orientation'` is a no-op on an image with no EXIF profile, and `semitransparent.png` was fully opaque, because `magick xc:'rgba(...)'` composites the alpha away unless the canvas already has an alpha channel. Both are now generated with Pillow and actually carry the property, and the scenarios assert the behavior instead of only the output format: an Orientation=6 JPEG must come out with its dimensions swapped, `--no-auto-orient` must leave them alone, and partial alpha must survive a PNG pass and flatten for JPEG.
- Get CI green again: `chunks_exact_mut(4)` in the SVG premultiply loop is now `as_chunks_mut::<4>()`, which the `clippy::chunks_exact_to_as_chunks` lint on current stable rejects, and `h2` moves 0.4.15 -> 0.4.19 for RUSTSEC-2026-0258. The remaining unpatched `h2` 0.3.27 is ignored with a rationale: it is reachable only as an outbound S3 client through the legacy AWS SDK hyper 0.14 path, the 0.3 line has no patched release, and truss's own inbound server does not use h2.

## v0.12.0

### Added

- WebP output carries ICC, EXIF, and XMP in `ICCP`/`EXIF`/`XMP ` container chunks, lossy encoding included. libwebp embeds no metadata, so truss now writes the chunks itself, promoting the container to the extended (`VP8X`) format when needed.
- E2E coverage (atago) for the output pixel budget, color-model preservation, and metadata retention, plus an `integration/fixtures/icc-profile.jpg` fixture carrying an embedded sRGB profile.

### Changed

- `--strip-metadata` now documents, in `truss help convert`/`help optimize` and the README, that lossy optimization keeps the ICC profile so colors are not shifted by the re-encode. The README also gains a per-format metadata support table.

### Fixed

- `convert`/`optimize`: enforce `MAX_OUTPUT_PIXELS` against the real output size when only one of `--width`/`--height` is given ([#252](https://github.com/nao1215/truss/issues/252)). The check previously used the *source* size for the omitted axis, so `--width 10000` on a small image produced a 100-megapixel output with exit 0, and a large enough single dimension stalled while allocating the resize buffer.
- `convert`/`optimize`: stop adding an alpha channel to opaque images ([#253](https://github.com/nao1215/truss/issues/253)). Every decoded image was widened to RGBA8 before encoding, so a no-op same-format pass flipped `hasAlpha` from `false` to `true` and grew the file. PNG, WebP, BMP, TIFF, and AVIF output now use an RGB color model whenever the pixels are fully opaque, and keep alpha whenever any pixel is not.
- `inspect`: report `hasAlpha` for lossless WebP (VP8L) instead of `null`; the `alpha_is_used` header bit is now read.
- `optimize --format webp --mode lossy` no longer fails on ICC-bearing input ([#279](https://github.com/nao1215/truss/issues/279)). The lossy encoder rejected any retained metadata, while a strip request is upgraded to "preserve ICC" for lossy output, so no flag combination worked — `--strip-metadata` was itself the reason the command failed.
- The lossy "preserve ICC" upgrade now applies only to formats that can carry a profile (JPEG, PNG, WebP). For AVIF it stays a full strip instead of putting the pipeline into a state the encoder rejects.

## v0.11.5

### Changed

- Batch dependency updates (consolidated from Dependabot PRs):
  - Rust: `hmac` 0.12 → 0.13 and `sha2` 0.10 → 0.11 (RustCrypto `digest` 0.11); `rav1d-safe` 0.3 → 0.5; `azure_storage_blob` 0.9 → 0.10 with `azure_core` 0.32 → 0.33; `tokio` 1.50 → 1.52, `uuid` 1.22 → 1.23, `aws-sdk-s3` 1.127 → 1.129, `google-cloud-storage` 1.9 → 1.10, `google-cloud-auth` 1.7 → 1.8, `libc` 0.2.183 → 0.2.186, `rand` 0.9.2 → 0.9.4.
  - Examples: `typescript` 5.9 → 6.0 (Next.js), `puppeteer-core` 24 → 25. `vite` kept on the 7.x line (7.3.5); Vite 8 drops the wasm-bindgen `new URL(..., import.meta.url)` asset reference and breaks the WASM example, so the upgrade is deferred.
  - GitHub Actions: `actions/configure-pages` 5 → 6, `actions/upload-pages-artifact` 4 → 5, `actions/deploy-pages` 4 → 5, `softprops/action-gh-release` 2 → 3.
- Pin integration-test container images to fixed versions (`adobe/s3mock` 4.11.0, `fsouza/fake-gcs-server` 1.54.0, `azurite` 3.35.0, `runn` v1.9.2) to avoid breakage from floating `:latest` tags. s3mock 5.0.0 changed `GET /<bucket>` to return an HTTP error instead of 200, which broke the s3 readiness probe.

### Fixed

- Update `rustls-webpki` 0.103.10 → 0.103.13 to patch RUSTSEC-2026-0098/0099/0104 (name-constraint bypass and CRL-parsing panic) on the modern rustls 0.23 path.
- Ignore RUSTSEC-2026-0098/0099/0104 for the transitive `rustls-webpki` 0.101.7 (legacy AWS SDK `rustls` 0.21 path; upstream has not migrated yet — same rationale as RUSTSEC-2026-0049).

## v0.11.4

### Changed

- Bump MSRV from 1.87 to 1.92.

### Fixed

- Update `aws-lc-sys` 0.38.0 → 0.39.0 to fix CRL Distribution Point scope check logic error and X.509 Name Constraints bypass via wildcard/unicode CN (high severity).
- Update `rustls-webpki` 0.103.9 → 0.103.10 to fix certificate revocation enforcement bug (medium severity).
- Ignore RUSTSEC-2026-0049 for `rustls-webpki` 0.101.7 (transitive dep via AWS SDK's `rustls` 0.21; upstream has not migrated yet).

## v0.11.3

### Added

- Port security and edge-case tests:
  - SSRF: redirect chain to metadata endpoint, scheme rejection (ftp/file/data), userinfo rejection, private IP/port blocking in strict mode.
  - Path traversal: E2E coverage for `../../etc/passwd`, mid-path dotdot, `.git` file content leak prevention.
  - Remote errors: upstream 4xx/5xx/403 mapped to 502, Content-Length exceeding limit returns 413, unsupported Content-Encoding (deflate, zstd) returns 502.
  - Image edge cases: corrupted/empty/truncated images return 415, ETag stability and divergence across processing options, ETag mismatch returns 200.
  - IP deny-list boundary tests: CGNAT, TEST-NET 198.18/15, broadcast, multicast, documentation ranges, IPv6 mapped/compatible/6to4/Teredo variants.
  - Path resolution: null byte injection, backslash literal on Unix, unicode filenames, very long components, multiple leading slashes, trailing dotdot.
  - Content-Encoding: multiple known encodings, mixed with unknown, whitespace handling.
  - Cloud metadata: GCP/AWS path variants, non-metadata IP allowed.

### Fixed

- Align crate, npm package, OpenAPI, example lockfile, and changelog release metadata for the `v0.11.3` release.

## v0.11.2

### Added

- Publish a production-oriented Next.js example that signs public truss URLs with `@nao1215/truss-url-signer`.

### Changed

- Verify Homebrew installs against `nao1215/tap/truss` during tagged releases and keep the formula layout aligned with `nao1215/homebrew-tap`.

### Fixed

- Align crate, npm package, OpenAPI, example lockfile, and changelog release metadata for the `v0.11.2` release.

## v0.11.1

### Added

- Publish `truss` Homebrew formulas from tagged releases to `nao1215/homebrew-tap` and verify installation on macOS.

### Changed

- Publish `@nao1215/truss-url-signer` from tagged releases via npm trusted publishing.
- Add README and deployment guide install paths for Homebrew and clarify the release prerequisites for the tap automation.

### Fixed

- Align crate, npm package, OpenAPI, example lockfile, and changelog release metadata for the `v0.11.1` release.

## v0.11.0

### Added

- Official `@nao1215/truss-url-signer` npm package source and release artifact flow for Node.js / TypeScript public signed URLs.
- Type definition compile checks plus Rust/Node compatibility coverage for `HEAD` signing, presets, and watermark parameters.

### Fixed

- Validate signed URL transform and watermark options in the TypeScript signer so it rejects server-invalid values before signing.
- Align crate, npm package, OpenAPI, and changelog release metadata for the `v0.11.0` release.

## v0.10.4

### Fixed

- Align crate, package, OpenAPI, and changelog release metadata for the `v0.10.4` tag after bootstrapping the npm package and trusted publisher settings.

## v0.10.3

### Changed

- Switch npm package publishing in GitHub Actions from `NPM_TOKEN`-based authentication to npm trusted publishing with GitHub OIDC.

### Fixed

- Align crate, package, OpenAPI, and changelog release metadata for the `v0.10.3` tag.

## v0.10.2

### Fixed

- Fix GitHub release workflow validation so the npm publish job no longer references `secrets` directly in an `if:` expression.
- Align crate, package, OpenAPI, and changelog release metadata for the `v0.10.2` tag.

## v0.10.0

### Added

- Official `@nao1215/truss-wasm` npm package source for third-party browser integration with a fixed `wasm,svg,avif` feature set.
- Release automation to pack the Wasm npm package, attach its tarball to GitHub Releases, and publish to npm when `NPM_TOKEN` is configured.

### Changed

- Expanded WASM documentation with npm package quick-start guidance, bundler-focused distribution details, build-mode differences, and local packaging instructions.
- Clarified the supported browser build matrix so AVIF support and WebP lossless behavior are explicit in the official package flow.

## v0.9.0

### Added

- Format-aware image optimization across the CLI, HTTP API, signed URLs, presets, and WASM with `optimize=auto|lossless|lossy` plus perceptual `targetQuality` controls.
- Optional Bearer token authentication for `/health` via `TRUSS_HEALTH_TOKEN`, while keeping `/health/live` and `/health/ready` unauthenticated for orchestrator probes (#73).
- Readiness probe hysteresis via `TRUSS_HEALTH_HYSTERESIS_MARGIN` to reduce flapping near disk and memory thresholds (#72).
- Additional fast coverage for lifecycle signal handling, public `HEAD` endpoints, and CLI runtime error paths.

### Fixed

- Gate AVIF/WebP native dependencies behind feature flags so the WASM build no longer imports unavailable C-backed components.
- Skip serializing transformed image bytes into WASM response JSON to avoid OOM on large outputs.
- Reject truncated JPEG input during lossless optimization.
- Stabilize HEAD and optimization-related tests after the runtime-target optimization work.

### Changed

- Consolidate project documentation under `docs/` and expand CLI examples for piping, stdin/stdout usage, and optimization workflows.
- Deduplicate cloud integration test helpers and parameterize HEAD request tests with `rstest`.
- Update the OpenAPI and configuration docs to cover optimization controls, `/health` authentication, and readiness hysteresis behavior.

## v0.8.0

### Added

- Lock-free syscall caching for health check endpoints (`disk_free_bytes`, `process_rss_bytes`) with configurable TTL via `TRUSS_HEALTH_CACHE_TTL_SECS` (default: 5s, range: 0–300). Eliminates redundant kernel context switches under high-frequency polling (#74).
- `ServerConfig::with_health_cache_ttl_secs()` builder method for programmatic TTL override.
- Per-IP rate limiting with sharded buckets to reduce mutex contention (#127).
- Reverse proxy support: resolve real client IP behind trusted proxies for rate limiting via `TRUSS_TRUSTED_PROXIES` (#117).
- `#[must_use]` annotations on key public types and functions (#130).
- `#[non_exhaustive]` on public enums for semver safety (#122).
- Integration tests for HEAD requests (#123).
- Unit tests for routing, signing, and inspect modules (#124).
- Non-ASCII input tests for `Rgba8::from_hex` (#131).
- Security audit CI on pull requests (#128).
- PR template and updated stale bug report placeholder (#126).

### Fixed

- Block SSRF bypass via IPv4-compatible, 6to4, and Teredo IPv6 addresses (#118).
- Add element count and nesting depth limits to SVG sanitizer; fix CSS `url()` search performance (#119).
- Disambiguate NUL escape to avoid clippy `octal_escapes` lint (#124).
- Guard `Rgba8::from_hex` against non-ASCII input (#131).
- Add `#[serial]` to cloud integration tests that use `env::set_var` (#116).
- Prevent flaky redirect-limit test on Windows (WSAECONNABORTED).
- Use acquire/release memory ordering in `HealthCache` for correctness on weakly-ordered architectures.

### Changed

- Extract `collect_resource_checks()` to deduplicate ~70 lines of identical logic between `handle_health()` and `handle_health_ready()`.
- Introduce unified transform dispatch to eliminate SVG/raster routing duplication (#115).
- Remove ~2400 lines of duplicated code from `server/mod.rs` (#114).
- Replace relay imports with direct submodule references in `auth.rs` and `metrics.rs`.
- Consolidate duplicated test helpers in CLI integration tests (#121).
- Replace manual JSON construction with `serde_json` in inspect command (#129).
- Throttle cache eviction scans and remove unnecessary `fsync` (#120).
- Hide `HealthCache` from public API; expose TTL via builder method.
- Document `TRUSS_HEALTH_CACHE_TTL_SECS`, `TRUSS_HEALTH_CACHE_MIN_FREE_BYTES`, and `TRUSS_HEALTH_MAX_MEMORY_BYTES` in `from_env` rustdoc.
- Update pipeline and Prometheus docs with crop/sharpen stages and watermark metric (#125).
- Bump clap 4.5→4.6, clap_complete 4.5→4.6, aws-sdk-s3 1.125→1.126.

## v0.7.2

### Fixed

- Fix aarch64 cross-compilation failure by using newer cross-rs base image with OpenSSL 3.x support.

## v0.7.1

### Added

- Hot-reload for transform presets via `TRUSS_PRESETS_FILE` with file-watching support.
- Dynamic log level switching via `TRUSS_LOG_LEVEL` env var and `SIGUSR1` signal.
- Unit and integration tests for log level and preset hot-reload.
- Crop, rotate, fit, and inspect examples to README.

### Fixed

- Use `saturating_duration_since` in rate limiter for Windows compatibility.
- Do not update `last_modified` on preset parse failure to handle torn reads.
- Use `wasm32-wasip1` C target for wasi-sdk sysroot header resolution in Pages CI.

### Changed

- Update `Cargo.toml` keywords for better crates.io discoverability.
- Comprehensive project improvements from multi-perspective review.

## v0.7.0

### Added

- Configurable max input pixel limit (`TRUSS_MAX_INPUT_PIXELS`) with 422 response for oversized images.
- Configurable max upload body size (`TRUSS_MAX_UPLOAD_BYTES`) with 413 response for oversized uploads.
- Optional Bearer token protection for `/metrics` endpoint (`TRUSS_METRICS_TOKEN`) and disable flag (`TRUSS_DISABLE_METRICS`).
- Configurable keep-alive max requests (`TRUSS_KEEP_ALIVE_MAX_REQUESTS`).
- Config validation subcommand (`truss validate`) for CI/CD pre-flight checks.
- Enhanced health checks: cache disk free space (`TRUSS_HEALTH_CACHE_MIN_FREE_BYTES`), transform capacity, and process memory usage (`TRUSS_HEALTH_MAX_MEMORY_BYTES`).
- Graceful shutdown with configurable drain period (`TRUSS_SHUTDOWN_DRAIN_SECS`); `/health/ready` returns 503 immediately on SIGTERM/SIGINT.
- Custom response headers via `TRUSS_RESPONSE_HEADERS` JSON env var with security-critical header rejection.
- Gzip response compression for non-image responses with configurable level (`TRUSS_COMPRESSION_LEVEL`) and disable flag (`TRUSS_DISABLE_COMPRESSION`).
- Crop control in the WASM demo page UI.
- SVG and lossy WebP features enabled in the WASM demo build.

### Fixed

- `Box::leak` per-request memory leak in custom response headers.
- Reject security-critical headers (framing, hop-by-hop) in `TRUSS_RESPONSE_HEADERS` at startup.
- Merge `Vary` headers into a single line to avoid duplication.
- Reduce worker drain timeout to 15 s for Kubernetes compatibility.
- Replace busy-wait accept loop with `poll(2)` on Unix.
- Windows graceful shutdown via SIGINT handler and draining check.
- Use `sigaction`, `AtomicI32`, `cast_mut`, and `O_NONBLOCK` on write fd for signal safety.
- Pixel-cap check moved before cache lookup to prevent unnecessary cache reads.
- Early-reject `/metrics` before body read.
- README: `--bearer-token` CLI flag corrected to `TRUSS_BEARER_TOKEN` env var.
- README: `POST /images:transform` curl example corrected to `POST /images` for multipart uploads.

### Changed

- OpenAPI spec documents HEAD method support on all GET endpoints.
- `UnprocessableEntity` response includes example in OpenAPI spec.
- `maxInputPixels` marked as required in `HealthDiagnosticResponse` schema.
- Extracted `parse_env_u64_ranged` helper for env var parsing.

## v0.6.2

### Fixed

- aarch64 cross-compilation failure: `Cross.toml` pre-build now installs `libssl-dev:arm64` instead of the host-architecture package, so `openssl-sys` finds the correct headers.

### Changed

- Release profile: enable thin LTO, single codegen unit, and binary stripping for smaller, faster binaries.
- Unified `stderr_write` usage across S3, GCS, and Azure backends to avoid Rust 2024 `ReentrantLock` issues with `eprintln!`.
- Cache key computation uses streaming `Sha256` hasher and inline parameter builder, eliminating intermediate allocations and sort.
- Watermark margin capped at 9999 with explicit validation on both JSON and multipart endpoints.
- Docker Compose healthcheck added for the `truss` service.

### Added

- Unit tests for `auth`, `http_parse`, `multipart`, `negotiate`, and `response` modules (314 new tests).

## v0.6.1

### Fixed

- HTTP response splitting (CRLF injection) via `X-Request-Id` header — CR, LF, and NUL bytes are now rejected.
- Integer overflow in AVIF decode when frame dimensions exceed address space (`width * height * 4`).
- aarch64-unknown-linux-gnu release build failure caused by missing OpenSSL (`Cross.toml` pre-build step).

### Changed

- Extracted `ServerConfig` and related types into dedicated `config.rs` module (~980 lines out of `mod.rs`).
- Deduplicated `read_remote_source_bytes` / `read_remote_watermark_bytes` into shared `fetch_remote_bytes` with `RemoteFetchPolicy`.
- Cleaned up unused imports in server module after config extraction.

### Added

- Integration tests: health endpoint 200, unknown path 404, CRLF injection prevention, missing Content-Type 415, invalid JSON body 400, missing source file 404.
- Characterization unit tests for `extract_request_id`, `ServerConfig` defaults/builder, `route_request`, and `TransformSlot` concurrency.

## v0.6.0

### Added

- Explicit crop operation (`--crop x,y,w,h` CLI flag, `crop` query parameter, JSON/WASM adapters). Applied after auto-orient and rotation but before resize. Not supported for SVG inputs.
- Signed URL key rotation via `TRUSS_SIGNING_KEYS` JSON env var. Multiple key IDs can be active simultaneously for zero-downtime key rotation.
- Server-side transform presets via `TRUSS_PRESETS` / `TRUSS_PRESETS_FILE` env vars with `preset` query parameter.
- Sharpen filter (`--sharpen` CLI flag, `sharpen` query parameter, WASM adapter) using unsharp mask. Valid sigma range 0.1–100.0.
- TIFF format support for input and output across CLI, HTTP server, and WASM.
- Watermark overlay support for signed public URLs (`watermarkUrl`, `watermarkPosition`, `watermarkOpacity`, `watermarkMargin` query params).
- `sign_public_url` and CLI `sign` command now accept watermark parameters.
- `truss_watermark_transforms_total` Prometheus counter.
- `watermark` field in structured access log entries.
- `MAX_WATERMARK_PIXELS` limit (4 MP) checked before watermark decode.
- Request deadline (60 s) caps total outbound fetch time per request.
- Origin cache namespace separation (`src:` / `wm:`) prevents cross-contamination.
- WASM UI: watermark file type validation, 10 MB size limit, loading/clear feedback.
- Integration tests for orphaned watermark params, empty URL, SVG + watermark rejection, and redirect following.
- Prebuilt release binaries with checksums for Linux (x86_64, aarch64), macOS (x86_64, aarch64), and Windows (x86_64).
- Multi-arch container images (amd64, arm64) published to GHCR on release.

### Changed

- Watermark fetch is deferred until concurrency slot is acquired (two-phase validation + fetch).
- SVG sources with watermark requests are rejected early with 400.
- Watermark fetch errors are sanitized; detailed errors logged server-side only.
- Cache key normalization uses parsed `Position` for consistent hashing.
- WASM UI: blur values below 0.1 treated as no blur; "Blur sigma" label simplified to "Blur".
- WASM UI: `.is-busy` scoped to interactive elements instead of entire page.
- WASM UI: download filename includes `-watermarked` suffix when applicable.
- Integration test workflow refactored from 4 duplicate jobs to a single matrix strategy.
- `Dockerfile.release` uses `COPY --chown` and explicit `chmod` for binary permissions.
- `parse_presets_from_env` treats empty `TRUSS_PRESETS_FILE` as unset; JSON parse errors include source info.
- `ServerConfig::PartialEq` compares preset contents instead of only length.

### Fixed

- Accessibility: `role="alert"` on error box, `:focus-within` on dropzones, `name` attributes on watermark inputs, `<noscript>` fallback.

## v0.5.0

### Added

- Prometheus `/metrics` endpoint with histograms (HTTP request duration, transform duration, storage duration) and error counters.
- Prometheus metrics documentation (`docs/prometheus.md`).
- Dedicated 304 status counter for cache-validation traffic tracking.

### Changed

- `/metrics` endpoint no longer requires bearer token authentication for Prometheus scraper compatibility.
- Cross-platform CI tests (macOS/Windows) now run on pull requests, not only on main pushes.
- Storage duration metrics now reflect actual source kind (filesystem/S3/GCS/Azure) instead of server config default.
- HTTP request duration histogram records on all exit paths including auth and body-read errors.

### Fixed

- Windows compilation error: `unsafe extern "system"` block for Rust 2024 edition.
- Cross-platform `stderr_write` using `GetStdHandle` on Windows.

## v0.4.0

### Added

- S3-compatible object storage backend (`--features s3`).
- Google Cloud Storage backend (`--features gcs`).
- Azure Blob Storage backend (`--features azure`).
- SSRF validation for S3/GCS/Azure backend endpoint URLs.
- Signed URL support for S3/GCS/Azure source images.
- Structured JSON access logs with request ID (`X-Request-Id`) and RAII concurrency guard.
- Configurable server concurrency and deadline limits.
- Startup health check for storage backends (fail-fast).
- Configurable storage timeout via `TRUSS_STORAGE_TIMEOUT_SECS`.

### Changed

- Bump `quick-xml` 0.37→0.39 and `resvg` 0.45→0.47.
- Azure environment variable renamed from `TRUSS_AZURE_BUCKET` to `TRUSS_AZURE_CONTAINER`.
- Use `subtle::ConstantTimeEq` for bearer token comparison.
- Graceful shutdown with 30-second deadline.
- Backend 401 responses mapped to 502 Bad Gateway.
- Health check name unified to `storageBackend` across all backends.
- Debug output masks `bearer_token` and `signed_url_secret` as `[REDACTED]`.

### Fixed

- Access-log latency measured after header read and after response write.
- Per-server in-flight counter and pool sizing.

## v0.3.0

### Added

- Blur filter support (`blur` query parameter) for image transforms.
- Watermark overlay support for image transforms.
- Sample image and template for documentation.

### Changed

- Refactored README for clarity.
- Optimized GitHub Actions workflows for faster CI.

### Fixed

- Blur cache key precision issue.
- SVG blur/watermark rejection handling.
- Watermark pixel limit validation.
- Relaxed watermark size check to match position-based margin usage.
- Pass watermark to `transform_svg` for proper SVG input rejection.
- Updated help text and OpenAPI spec for blur/watermark options.
- Update OpenAPI spec version from 0.2.0 to 0.3.0.

## v0.2.0

### Added

- HTTP/1.1 keep-alive and HEAD method support for CDN origin use.
- SVG rasterization and input-format preservation in Accept negotiation.
- `TRUSS_DISABLE_ACCEPT_NEGOTIATION` flag to avoid CDN cache key mismatches.
- Configurable `Cache-Control` max-age / stale-while-revalidate via environment variables.
- Signed URL support for public GET endpoints (`GET /images/by-path`, `GET /images/by-url`).
- Download counter.
- Benchmark results to README.
- CDN architecture documentation and cache key configuration guidance.
- Mobile-friendly WASM demo with aspect ratio lock.
- Edge case tests.
- `truss help completions` and `truss help version` help topics.
- Shell completions now expose implicit-convert (`-o`, `INPUT`) and implicit-serve (`--bind`, `--storage-root`) arguments.
- Commands table and shell completion setup guide in README.
- Exit code 5 (runtime error) documented in `--help` exit code listing.

### Changed

- Refactored `server.rs` into 9 sub-modules for maintainability.
- Normalize default fit/position in cache key for better hit rate.
- Authenticate private POST routes before reading request body.
- Use unique temp-file suffix for concurrent cache writes.
- Accept negotiation uses specificity to break ties (e.g. `image/png` over `image/*`).

### Fixed

- Validate multipart boundary suffix to prevent payload collision.
- Apply rotation in SVG rasterization path.
- Treat extensionless files as implicit `convert` input; use `is_file()` to exclude directories.
- Reject Transfer-Encoding header to prevent request smuggling.
- Warn at startup when signed URL credentials are set without `TRUSS_PUBLIC_BASE_URL`.
- Accept `Authorization: bearer` (case-insensitive scheme) per RFC 7235.
- Preserve tail bytes for keep-alive connections instead of truncating.
- Reject header names with leading/trailing whitespace.
- Enforce `MAX_HEADER_BYTES` at header terminator, not just buffer size.
- Handle weak ETags (`W/"..."`) in `If-None-Match` comparison.
- Only treat 2xx HTTP responses as successful remote fetches.
- Block IPv4-mapped IPv6 addresses (`::ffff:127.0.0.1`) in SSRF check.
- Correct inverted `data:image/*` allowlist in SVG sanitizer; `data:image/png` etc. were incorrectly blocked while `data:image/svg+xml` was incorrectly allowed.
- Clamp aspect-ratio synced dimensions to minimum of 1 in WASM demo.
- Reduce idle timeout for unconsumed fixture responses to speed up tests.
- Map `InvalidOptions` to exit code 1 (usage) and `InvalidInput` to exit code 3 (input); previously both mapped to exit code 2 (I/O).
- Map output file write failure to exit code 2 (I/O) instead of exit code 5 (runtime).
- Use Drop guard for `TRANSFORMS_IN_FLIGHT` in backpressure test to prevent flaky parallel test failures.
- Update OpenAPI spec version from 0.1.0 to 0.2.0.

### Security

- Sanitize SVG `href`/`xlink:href` with allowlist approach; block embedded SVG payloads.
- Validate remote fetch targets against SSRF policy before serving cached responses.
- Reject whitespace-padded HTTP header names to prevent proxy interpretation differences.

## v0.1.0

- Initial release.
