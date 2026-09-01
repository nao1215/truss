# Changelog

## [Unreleased]

### Fixed

- A cache eviction scan runs once a minute, as it was documented to, rather than on every write ([#423](https://github.com/nao1215/truss/issues/423)). The scan walks the whole cache directory, stats every file, and sorts them, and the timestamp meant to hold it to one pass per minute lived on a `TransformCache` value the server builds fresh for each request, so it started at zero every time and the guard never fired. On a root holding 60,000 entries, which a site with a few thousand images at a handful of sizes reaches within the hour that entries live, that put 385 to 493 ms of directory walking in front of a 2 ms transform, on the request thread, inside the transform deadline and inside a transform slot; unsetting `TRUSS_CACHE_MAX_BYTES` against the same root returned the same request to 3 ms, which is to say that turning on the disk-size guard `docs/configuration.md` tells operators to set was what made the server slow. The timestamp now lives on the server and is shared by every request, so the throttle bounds the walk as described and the `compare_exchange` that was meant to let one thread win a scan is comparing against a value another thread can have set. Nothing about which entries are evicted changes: the scan still removes the oldest first until the total is under the budget.

- An interrupted cache write no longer strands bytes that nothing counts and nothing removes ([#424](https://github.com/nao1215/truss/issues/424)). A cache entry is written to a temporary file and renamed over the destination, and the eviction scan skips any name containing `.tmp.` so that it cannot delete a write another thread is in the middle of. A write that never reaches its rename, which is what a `SIGKILL`, an OOM kill, or a container terminated past its grace period leaves behind, produced a file matching that same rule, so its bytes were neither counted toward `TRUSS_CACHE_MAX_BYTES` nor ever reclaimed: a root holding one 500,000 byte orphan sat at 504,575 bytes against a 50,000 byte budget while every scan reported the cache as under it, and each subsequent stop added another. Age now separates the two cases the name could not: a temporary file younger than the cache TTL is skipped as before, and one older than that, which no live writer can be holding, is removed by the scan. The margin is deliberately wide, both because deleting a file a live writer is about to rename is the expensive mistake and because a second truss process may share the root.

- `optimize` returns the input rather than a larger re-encode for a WebP as well ([#420](https://github.com/nao1215/truss/issues/420)). The rule that an optimization never hands back more bytes than it was given was established for JPEG in v0.16.0 and extended to the passthrough's other conditions since, and the candidate list it chooses from had an arm for JPEG and one for PNG and none for WebP. The lossless WebP encoder truss reaches through the `image` crate is a plain one, so a picture libwebp compressed well came back two and a half times its size — the 94 byte fixture at `integration/fixtures/libwebp-lossless.webp` became 234 bytes under `--mode lossless` and 128 under `auto` and `lossy` — at exit 0 with nothing on stderr, which is the whole of what a script running the command over an asset directory would have seen. All four adapters were affected, since the rule is in the codec. A WebP now offers its own bytes as a candidate the same way, and where the metadata policy would strip something the file carries, the container is rewritten without those chunks rather than the file being disqualified: `ICCP`, `EXIF`, and `XMP ` are the three the WebP specification names, so dropping the ones the policy removes is a decision about a closed set, unlike PNG's open-ended ancillary chunks, and the `VP8X` flags that advertise them are cleared with them. A WebP whose EXIF orientation has to be applied to the pixels still re-encodes and can still come out larger, because its pixels are not the pixels it was given; `--keep-metadata` returns such a file untouched, as it did before.

## v0.20.0

### Changed

- The watermark opacity rule is written once and the server's own margin cap is gone ([#402](https://github.com/nao1215/truss/issues/402)). The range 1 to 100 was restated in the core, the CLI, the Wasm package, both server entry points, and the npm signer, in four different spellings, so a caller who sent `watermarkOpacity=101` on a public URL was told about `watermark.opacity`, a field only the JSON body has. The four Rust copies now read one predicate in the core and print the sentence the CLI and the Wasm package already printed, `watermark opacity must be between 1 and 100`, while each adapter keeps its own failure class; the messages for the watermark url and position on the shared JSON-and-query path lost their dotted prefix for the same reason. The server's ceiling of 9999 on the margin is removed: `apply_watermark` already refuses any margin that leaves the watermark no room and names the sizes involved, so the cap only decided which of two classes the caller saw for the same picture, `invalid-request` at 10000 and `invalid-options` at 9999, and it existed on no other adapter, which is why `truss sign --watermark-margin 10000` and the npm signer both emitted a URL the server refused. `docs/openapi.yaml` documented `watermarkMargin` with no maximum throughout, so the implementation now matches what was published. A margin that does not fit is still refused, with the pipeline's message and the class every adapter reports.

- The server admits one transform per core rather than 64 on every machine ([#397](https://github.com/nao1215/truss/issues/397)). A transform is CPU work from end to end, so admitting more of them than the machine can run does not make the server faster. Measured on a 32-core machine with AVIF output, throughput was the same at 32 in flight and at 64, while the median latency went from 19 to 30 seconds and the 95th percentile from 25 to 79; against the 30 second default deadline that is not merely slower, it is a failure. A run of 128 uploads at 64 in flight against a stock server answered 62 of them and failed 66 with `413`, each after its encode had already finished, so the work was done and thrown away. The excess is now turned away at admission with the `503` the server already had for a full slot pool, which says retry rather than too large and says it before the CPU is spent. `TRUSS_MAX_CONCURRENT_TRANSFORMS` still overrides the default and still accepts 1 to 1024, so a deployment that has measured its own saturation point keeps whatever it set; a cache hit is answered before a slot is taken, so nothing the cache can serve is affected. A machine that will not report its core count falls back to 4.

### Breaking Changes

- `targetQuality` takes its metric in one spelling, `ssim` or `psnr`, rather than in any case ([#416](https://github.com/nao1215/truss/issues/416)). Every other named value truss takes is matched as written: `--fit COVER`, `--position CENTER`, `--format JPEG`, and `--mode LOSSLESS` are all refused, and the metric half of `--target-quality` was the one place that lesson did not hold, so a caller learned a rule from one flag that did not carry to the next. `SSIM:0.98` is now `unsupported target quality metric \`SSIM\``, the message an unknown metric has always had, on the CLI, in the HTTP server, and in the Wasm package alike, since all three read the one parser. `docs/openapi.yaml` already published the field with a lowercase-only pattern, so the implementation was the lenient one rather than the specification. A script or a hand-built signed URL that spells the metric in any other case needs it lowercased; `--background FF0000` is unaffected and stays case-insensitive, because that is a hexadecimal number rather than a name, where `ff0000` and `FF0000` are one value rather than two spellings.

### Fixed

- An optimization keeps the ICC profile under `--strip-metadata`, whichever mode asked for it ([#418](https://github.com/nao1215/truss/issues/418)). The upgrade from "strip everything" to "keep the profile" was written for `lossy` alone, so `truss optimize photo.jpg -o out.jpg`, which is `auto` and is the command the README documents, dropped the profile that `--mode lossy` on the same file kept, and a wide-gamut photograph came out rendering in the wrong colors with nothing said about it. `auto` reaches the same lossy encoder for JPEG, WebP, and AVIF output, and a lossless optimization re-encodes as well; the pixels surviving does not help, because the profile is what says how to read them. The condition is now that an optimization was asked for at all, which is the same predicate #356 settled on for the passthrough. `OptimizeMode::None` is unchanged and still strips, so a plain `truss convert photo.jpg -o out.jpg` behaves as its flag says and as it always has. Formats that cannot carry a profile are unchanged too, since the upgrade has always been limited to those that can, and `--preserve-exif` still drops the profile under every mode. An output gains the profile's bytes, which for a small image is a large fraction: the 588 byte profile in `integration/fixtures/icc-profile.jpg` against a 758 byte optimized output. That is the trade `--mode lossy` has been making since v0.16.0, and where the input already carries the profile the passthrough now returns the input untouched rather than re-encoding it. A server with a transform cache keeps serving what it stored for a request until that entry is evicted, so an operator who wants the profile back on already-cached URLs has to clear the cache: the key covers the request, which has not changed, and not the answer, which has.

- Metadata a TIFF or BMP output cannot carry is reported rather than dropped in silence ([#419](https://github.com/nao1215/truss/issues/419)). `truss convert photo.jpg -o out.tiff --keep-metadata` exited 0 and wrote a file with neither the EXIF nor the ICC profile it was told to keep, and `--preserve-exif` did the same, while the same request for XMP or IPTC raised a warning and the same request for AVIF failed outright. One situation, metadata the output format cannot carry, had three answers, and the quietest one covered the two kinds callers ask for most. The format's capability table is now read once, after the policy has narrowed the metadata to what the caller asked for, and every kind that cannot travel becomes a `MetadataDropped` warning: a `warning:` line on stderr, a `Truss-Warning` header from the server, and an entry in `warnings` from the Wasm package. That also removes the copy of the per-format lists that the warning code kept beside the retention code. Nothing changes for JPEG, PNG, or WebP, which carry what they are given, and AVIF still refuses rather than warns, which is worth revisiting on its own.

- Both signers refuse an empty key id, secret, or source rather than minting a URL no server can accept ([#412](https://github.com/nao1215/truss/issues/412)). A server refuses to start with an empty key id or secret in `TRUSS_SIGNING_KEYS`, and the public routes refuse an empty `path` or `url`, so each of those URLs is answered 401 or 400 for as long as it exists; `truss sign` produced one at exit 0 for all three, and `@nao1215/truss-url-signer` produced one for the empty secret while refusing the other two, so the two signers also disagreed about the same input. This is the rule #405 established for the transform options, applied to the credentials and the source: a signed URL is usually written somewhere other than where it is fetched, into a build step, a database, or an email, so a URL that can never work has to fail at the process that mints it. It matters most for the shape this command has in a build script, `truss sign --key-id "$KEY_ID" --secret "$SECRET" --path "$IMAGE_PATH"`, where an unset variable expands to the empty string rather than failing. One predicate in the signing module now answers for all three and both signers read it, `truss sign` reporting `error: key id must not be empty (invalid-request)` at exit 1 with nothing on stdout, and the npm package throwing in its own vocabulary as it already did for `keyId` and `path`.

- Both signers keep a path in the base URL, so a deployment served under a prefix can be signed ([#413](https://github.com/nao1215/truss/issues/413)). `truss sign --base-url https://cdn.example.com/img` and `signPublicUrl({ baseUrl: "https://cdn.example.com/img" })` each returned `https://cdn.example.com/images/by-path?...` with the `/img` silently gone, so the URL pointed at a path the CDN does not serve while carrying a signature that verifies; the failure showed up as the CDN's 404 at whoever loaded the image. Both signers now join the endpoint onto the base URL's path instead of replacing it. No signature moves: the canonical string's `REQUEST_PATH` is the literal endpoint path truss receives after the proxy has stripped the prefix, which is what `docs/signed-url-spec.md` has always specified, and both signers were reading it off the URL they were about to emit, which happened to be the same string only because the prefix was being discarded. A base URL with no path, or with just a trailing slash, produces exactly the URL it did before.

- `truss serve` and `truss validate` run the storage check the library entry point runs ([#414](https://github.com/nao1215/truss/issues/414)). `truss::adapters::server::serve` verifies the storage before it accepts a connection, and neither CLI command did, so a storage root that resolved to a file bound the port and answered `500 failed to access source artifact: Not a directory` to every transform while `/health/ready` reported `storageRoot: fail`, and `truss validate` printed `configuration is valid` for the same configuration. A storage root that does not exist was already caught, because the configuration parser canonicalizes it, which is why the case that survived is the one where the path resolves and is not a directory: a container whose volume mount landed on a file. The check also carries the S3, GCS, and Azure reachability probes, and every published binary and the container image are built with all three, so a mistyped bucket or an expired key had the same shape. Both commands now refuse such a configuration with exit 1, the code the CLI already gives a configuration fault, naming the storage root or the backend. `truss validate` against a cloud backend now reaches the network, which `docs/api-reference.md` says.

- `truss convert` and `truss optimize` replace an output file through a temporary file, so a write that fails partway cannot destroy the image that was there ([#408](https://github.com/nao1215/truss/issues/408)). `fs::write` opens the destination with `O_TRUNC` and then writes, so a full disk, a filled quota, or any failure after the open left a file shorter than the picture it had held, while the message on stderr said the write had failed and implied nothing had changed. The truncated file still carries a valid header, so `file` and `truss inspect` both describe it as the image it was going to be, and a later pass that checks the destination looks reasonable finds nothing wrong; converting in place, which is what `truss optimize photo.jpg -o photo.jpg` over a directory of photographs does, made that the only copy. The bytes now go to a temporary file beside the destination and are renamed over it once all of them are written, which is atomic within a directory and is what `server::cache` has always done with a cache entry. The mode of an existing destination is copied onto the replacement, since a new file would otherwise carry the umask instead of the permissions the destination was given, and a directory that will not take a temporary file falls back to the direct write it did before, so nothing that worked stops working. Writing to standard output is unchanged.

- A non-UTF-8 argument no longer aborts the process ([#410](https://github.com/nao1215/truss/issues/410)). A file name is a byte string on Linux with no encoding requirement, and truss read its arguments as text, so a photograph named in Latin-1, which is what a file unpacked from an old archive often is, panicked with a Rust backtrace note and exit 101, a code the documented set of six does not contain. It happened while the arguments were being collected, before any subcommand was chosen, so `truss inspect` and `truss --help` went the same way, and a glob was enough to reach it: one badly named file broke `truss convert *.png` for every other file in the directory. Paths are now carried as the bytes they are from `main` through to the file system, and such a file converts like any other; on macOS, where APFS refuses to create a name that is not valid UTF-8, the same argument is reported as the file system error it is rather than a panic. A value that is genuinely text and is not, `--format` or `--bind`, is still refused, now with the usage error and exit 1 that every other bad flag value gets. `truss::adapters::cli::run` takes anything that converts into an `OsString`, which the `Vec<String>` a crate caller passes today still does.

- The class of a failure is the last thing on the stderr line, whatever the message came from ([#407](https://github.com/nao1215/truss/issues/407)). `truss convert` prints `error: <message> (<class>)` and the message can come from a decoder in a dependency, whose wording truss does not choose; the JPEG decoder's ends with a newline, so a truncated upload printed the class alone on a second line, where a caller reading the last line of stderr finds no failure in it. The same string is the `detail` of the server's RFC 9457 body, where it ended with a literal `\n`, and the `message` `@nao1215/truss-wasm` hands to a browser. A message that leaves its line is now folded onto one where each adapter renders it, by one function in the core rather than at the thirty call sites a foreign error can enter from, so a dependency that starts or stops ending its wording with a newline in a version bump cannot reach the terminal either way; `server::cache` already did the same to a warning before putting it on an entry's header line. A message that is already one trimmed line is untouched, and no wording changes.

- A `--watermark` file that is not there is reported as `not-found` rather than `internal-error` ([#409](https://github.com/nao1215/truss/issues/409)). Both arguments name a source file on the command line, and a missing input was already `not-found`, the class `docs/problems.md` describes as a file the request named that does not exist, while a missing watermark was the class that says truss itself failed in a way the request did not cause. The class is what a caller branches on since v0.19.0, so a script that retries an `internal-error` and reports an `invalid-request` to whoever typed it did the wrong thing for a typo in a path. The rule that turns a file system fault into a class now lives in one function that both reads use, so a watermark that exists and cannot be read is still `internal-error`, which is what that class is for. Exit code 2 and the message wording are unchanged.

- The public GET routes refuse an option set no input could make valid before they ask the cache or fetch the source ([#401](https://github.com/nao1215/truss/issues/401)). `quality=101`, `width=0`, `blur=200`, `sharpen=500`, a `fit` or `position` without both dimensions, and `withoutEnlargement` without either are all decided from the query alone, but each was checked inside `TransformOptions::normalize`, which runs in the transform after the bytes are in hand. A `by-url` request carrying one of them made an outbound request to somebody else's server, a storage-backed one made a billed GET, and both were then answered `502 Bad Gateway`, telling the caller the origin had failed when the request had been wrong before it was sent. The same ordering made the answer depend on the cache: `fit` and `position` are the two options the cache key leaves out when width and height are not both present, so a request carrying an invalid `fit` hashed to the key of the valid request without it and was answered `200` with an image once any equivalent request had written the entry, while a cold cache answered `400`. The input-independent rules now live in one function that `normalize` runs first and that the server runs while reading the options, so the refusal arrives with the same message and the same `invalid-options` class before anything is fetched or looked up. Nothing changes for a valid request, and the rules that need the resolved output format, which for an absent `format` comes from the input, still run where they did.

- `truss sign` refuses an option set the server would always refuse, instead of minting a signed URL that answers 400 ([#405](https://github.com/nao1215/truss/issues/405)). `--fit cover` without both dimensions, `--position center` without both, `--quality 101`, and `--width 0` each produced a URL at exit 0 whose signature was valid and whose request was not, while `@nao1215/truss-url-signer` refused all four at the call site. A signed URL is usually written somewhere other than where it is fetched, into a build step, a database, or an email, so the failure surfaced at whoever loaded the image with no trace of the process that wrote it. The signer now reads the same list of input-independent rules the server does and reports the failure as `error: fit requires both width and height (invalid-options)` at exit 1, with nothing on stdout. Rules that depend on the source image are still the server's, since a signer has no image: a `crop` outside the picture and a watermark margin that leaves no room are refused at request time as before. `docs/signed-url-spec.md` now describes both signers rather than only the npm package.

- The cloud metadata block catches three spellings of the endpoints it refuses ([#403](https://github.com/nao1215/truss/issues/403)). `metadata.google.internal.`, with the trailing dot that makes a domain name absolute and resolves identically, was a different string from `metadata.google.internal` and matched nothing; `::169.254.169.254` and `2002:a9fe:a9fe::` carry the same address as the IPv4-mapped `::ffff:169.254.169.254` that the check already decoded. `docs/configuration.md` says these endpoints are blocked regardless of `TRUSS_ALLOW_INSECURE_URL_SOURCES`, and in the default configuration the promise held for a different reason, since all three resolve to a link-local address the deny-list refuses. With the insecure flag set, which the storage-emulator instructions tell people to do, the metadata check is the only rule left and these walked past it. The trailing dot is now normalized away before the comparison, and the three IPv6 encodings that carry an IPv4 address are decoded by one function that both the metadata check and the deny-list read, so the two cannot disagree again about how many ways an address can be written. A hostname under the caller's control that resolves to a metadata address is still outside what a name check can catch, and is refused by the deny-list whenever the deny-list runs.

- Four rows of `truss serve --help` printed flush against the left margin ([#404](https://github.com/nao1215/truss/issues/404)). `TRUSS_S3_BUCKET`, `TRUSS_GCS_BUCKET`, `TRUSS_AZURE_CONTAINER`, and `TRUSS_STORAGE_TIMEOUT_SECS` are the first row of their feature-gated block, and a Rust string literal opened with a backslash line continuation drops the newline together with the leading whitespace of the next line, so each block lost the indentation of its first row and the column the reader scans broke four times. Every published binary and the container image are built with `s3,gcs,azure`, so this is what a release user saw. The blocks now start the literal at the content rather than at a continuation, and a unit test asserts the shape of the whole section so a block added later is covered too.

- An AVIF encode picks its speed from the output size, so a large one finishes inside the transform deadline ([#396](https://github.com/nao1215/truss/issues/396)). rav1e's speed scale runs from 1 to 10 and truss asked for 4 at every size, which is a reasonable setting for a small image and a poor one at the ceiling `MAX_OUTPUT_PIXELS` allows: an 8192x8192 AVIF took 55 seconds on two cores against a 30 second default deadline, or 218 seconds for a source the encoder finds hard, and an ordinary 4000x3000 photograph took 58. All three now finish in 19, 19, and 15 seconds. Nothing changes at or below 2 megapixels, where a small output is quick at speed 4 and a faster setting does not reliably make it smaller: speed 6 came out 1.3 percent larger on a 1.7MP gradient and 10 percent larger on a 0.3MP image of noise, and there is no deadline to save there. Above that the trade turns over, because the alternative is a request that does not finish; the cost is up to 3.5 percent more bytes on a source the encoder finds hard, while ordinary content comes out smaller as well as faster, at 23,925 bytes against 25,885 for a 12MP gradient. `optimize` still runs two steps slower than the plain path at the same size, which below 2 megapixels is the speed 2 it already used. The deadline itself is unchanged and still cannot interrupt an encoder that has started; `docs/pipeline.md` now says so where it describes the stage.

- The criterion benchmarks measure the work they are named after ([#398](https://github.com/nao1215/truss/issues/398)). Every case that named an image size in its id read `integration/fixtures/sample.jpg`, which is 4 by 3 pixels, so `format_conversion/jpeg_to_avif/640x427` encoded twelve pixels and reported 238 microseconds for it. The suite measured per-call setup and nothing else, which is why it ran green through every release that shipped the single-threaded AVIF encoder: an eleven-fold regression on real images is invisible on an image with no parallelism to exploit. Each case that touches pixels now builds its own source at the size its name gives, a gradient with a fine texture over it because the encoders are content-sensitive and a flat image is the cheapest thing they will ever be handed, and the generator reads the size back out of the encoded bytes so a name and an image cannot drift apart again. The same case now reports 328 milliseconds. The format conversion group takes twenty samples rather than a hundred, since an AVIF encode of that size would otherwise put the group into the minutes on its own. `sniff_artifact` and `png_to_jpeg` keep their fixtures, which are the specific files those cases are about. `docs/development.md` gains the current medians as a baseline, and marks the AVIF rows of the older CLI table as predating the threading change in v0.19.0. The build workflow also watches `benches/**`, which it did not: `cargo clippy --all-targets` and `cargo test --all-targets` both compile the benchmarks, but no path filter named the directory, so a change to a benchmark ran no job at all.

## v0.19.0

### Added

- Transform warnings reach an HTTP client as `Truss-Warning` response headers, one per warning ([#381](https://github.com/nao1215/truss/issues/381)). The pipeline raises a warning for a result that is not quite what was asked for, an EXIF orientation dropped with `autoOrient=false`, a `targetQuality` the encode could not reach, metadata the output format cannot carry, and the CLI printed each as `warning:` and the Wasm package returned them in `warnings`, but the server wrote them to its own log and sent the image with a plain 200, so the adapter whose caller is another process was the one that kept them to itself. The header carries the same text the other two adapters do, as one line of visible ASCII. The cache entry keeps the warnings on its header line, tab-separated after the media type, so a hit repeats the headers the miss produced; an entry written before this change has no such fields and reads as warning-free, so nothing is evicted. A 304 carries none, since it says the client already has the representation. The HTTP `Warning` field is not used because RFC 9111 deprecates it, and the name has no `X-` prefix because RFC 6648 retired that convention.

### Changed

- `truss` names the class of a failure on stderr, as `error: <message> (<class>)` ([#383](https://github.com/nao1215/truss/issues/383)). The class was already the one thing the three adapters agreed on: the HTTP server puts it in the RFC 9457 `type`, `@nao1215/truss-wasm` reports it as `kind`, and `docs/problems.md` describes each one once. The CLI was the adapter that named it nowhere, so a caller who had learned that a `fit` without both dimensions is `invalid-options` on the server and `invalidOptions` in the browser got `exit 1` and a sentence from the CLI, and the only way to tell that failure from an unparsable command line, which is also exit 1, was to match on wording `docs/problems.md` says is not part of the API. The class now follows the message in parentheses: `error: fit requires both width and height (invalid-options)`. All three spellings come from one table in the core, joined to the pipeline's errors by an exhaustive match, so a class cannot be added to one adapter and missed in the others, and `docs/problems.md` gains a row per class carrying its CLI exit code, HTTP status, and Wasm `kind`. Exit codes are unchanged except where they were wrong (below), the message text before the parentheses is unchanged, and the usage and hint lines below it are unchanged; a script that matches a whole stderr line exactly needs its pattern extended, one that greps for a phrase or reads `$?` does not.
- HTTP error responses follow RFC 9457, with a `type` that names the class of failure ([#379](https://github.com/nao1215/truss/issues/379)). Every error was `application/problem+json` with `"type": "about:blank"`, which RFC 9457, the successor to the RFC 7807 the server cited, defines as "the status code is all there is to say"; the server used it for a 400 that meant `fit requires both width and height` and a 400 that meant the input could not be decoded alike, so a client could classify a failure only by parsing `detail`, which the RFC says not to do. `type` is now a URI naming the class, an anchor on the new `docs/problems.md` page that lists every class with its status and when it is sent, and `title` is the fixed name of that class. The transform classes carry the names `@nao1215/truss-wasm` reports as `kind`, so the two adapters classify a failure the same way. The body also gains `requestId`, RFC 9457's extension mechanism, repeating the `X-Request-Id` header so a body kept on its own can be matched to the access log. A route that does not exist keeps `about:blank`. Status codes, `detail` wording, and the `WWW-Authenticate` and `Retry-After` headers are unchanged.

### Breaking Changes

- An `Accept` header of `*/*` keeps the input format instead of selecting AVIF ([#392](https://github.com/nao1215/truss/issues/392)). RFC 9110 section 12.5.1 defines a request with no `Accept` as one that accepts any media type, which is what `*/*` says, but truss answered the two differently: a missing header kept the input's format and `*/*` was treated as a match for everything and resolved to the first format the server prefers, which is AVIF. A caller with a default HTTP client, `curl` included, sends `*/*` and asked for nothing, and was transcoded to the most expensive format truss encodes. A winner matched only by a `*/*` range is now read as no preference, and the request keeps the input format the way an absent header always did. A browser fetching an `<img>` sends `image/avif,image/webp,image/apng,*/*;q=0.8`, whose winner is matched by an exact range, and negotiates exactly as before, as does `image/*` and any header that names a type. `TRUSS_FORMAT_PREFERENCE` orders the formats a request asked for and no longer applies to a request that asked for none; a deployment that relied on `*/*` clients being transcoded should name `format` in the request or set it on the CDN. `Vary: Accept` is unchanged, and the cache key already carried the resolved format, so no stored entry becomes wrong.
- `POST /images` and `POST /images:transform` reject a query string with 400 instead of ignoring it ([#393](https://github.com/nao1215/truss/issues/393)). Both endpoints take their options from the request body and read no query parameters, and a query string on either was parsed by nobody: `POST /images?width=64&height=32&format=png` answered 200 with an untransformed image, having silently dropped four of the five things it was asked. The names dropped are the same names that mean something on the public GET routes, where an unknown one is already refused, so the caller most likely to write them is one moving a request from a signed URL. The refusal names the parameters that were sent and points at the body, which is what the multipart parser already does with an unrecognised form field. A request with no query string is unaffected, and so is every other route.
- `TransformOptionsPayload::rotate` is an `Option<i32>` rather than an `Option<u16>`. The field is a whole number of degrees with a sign, which is what `Rotation` has always taken and what the CLI and the Wasm package already accepted; the narrower type was the only thing refusing a counter-clockwise turn. A caller of the crate that builds the payload with a `u16` literal needs the literal widened, and one that reads the field back gets an `i32`. The JSON on the wire is unchanged for every value that worked before.
- `@nao1215/truss-wasm` reports a `format` it cannot write as `kind: "unsupportedOutputMediaType"` rather than `"invalidOptions"`. It is the same refusal with the same message, under the name the HTTP server puts in its problem `type` and the CLI prints in parentheses, so one classification now covers the three adapters. A browser client branching on `invalidOptions` for `format: "gif"` needs the other name; every other `invalidOptions` case is unchanged.
- A decode failure raised by the media sniff exits 4 rather than 3. `decode-failed` is exit 4 wherever the pipeline raises it, and the sniff that runs before the transform was the one call site that reported it as an input error, so a truncated PNG was a transform error to `truss convert` and an input error to `truss inspect`. A script that treats 3 and 4 differently and feeds truss files that are recognisable but undecodable sees the change; an unsupported format and a file with no recognisable signature still exit 3.
- The `type` member of every HTTP error body is a URI rather than `about:blank`, and the `title` of the transform-error bodies is the class name rather than the HTTP reason phrase: `Invalid transform options` where a 400 for bad options said `Bad Request`. A client that compared `type` to `about:blank`, or `title` to the reason phrase, needs to compare `type` to the class URI instead; `status` and `detail` are as they were. A route that does not exist still answers with `about:blank`.

### Fixed

- AVIF encoding uses every core the machine has ([#391](https://github.com/nao1215/truss/issues/391)). `image` reaches rav1e through `maybe-rayon`, whose functions are no-op shims until `ravif/threading` is enabled, and truss enabled the AVIF encoder without it, so every AVIF encode ran on one core no matter how many the host had. Encoding a 4000x3000 photo took 112 seconds on a 32-core machine, holding one core at 99 percent while the rest sat idle; it now takes 9.9 seconds, and the bytes are identical, because rav1e produces the same bitstream whatever the thread count. On the server this is what made the transform deadline look broken: the deadline is read at pipeline stage boundaries and an encode already under way is not interrupted, so an ordinary upload spent nearly two minutes in the encoder and then failed with `transform exceeded 30s deadline after encode`, holding one of the 64 transform slots the whole time. The rustdoc on the deadline and the `TRUSS_TRANSFORM_DEADLINE_SECS` row in `docs/configuration.md` now say that the check happens between stages rather than inside one, which is what the code has always done. The `avif` feature carries `image/rayon`, and a unit test reads the manifest and fails if it is dropped, since nothing in the encode path can assert it at run time.
- `rotate` takes a negative angle on the HTTP server, the way it already did on the CLI and in `@nao1215/truss-wasm` ([#385](https://github.com/nao1215/truss/issues/385)). `--rotate -90` is the counter-clockwise quarter turn the CLI documents and the browser option takes as an `i32`, but the JSON payload and the query parser both read the field as an unsigned 16-bit integer, so the server answered 400 before any truss code ran, with serde's `invalid value: integer -90, expected u16` naming a Rust type at the caller. The query path had a message of its own, `rotate must be 0, 90, 180, or 270`, which stopped being true in v0.13.0 when any whole angle became acceptable, so a caller who read it and sent `rotate=45` was told something false by the error and something else by the server that accepted it. Both paths now read the field through `Rotation`, which is where the rule has always lived: any whole number of degrees, negatives turning counter-clockwise and angles past a full turn wrapping, and one message for a value that is not a whole number. The signer is unaffected, because `Rotation` normalizes before the URL is written, so `truss sign --rotate -90` has always emitted `rotate=270`; a hand-built signed URL may now carry either spelling, as long as it is signed as sent.
- A `format` truss reads but cannot write is refused from the options rather than from the encoder ([#386](https://github.com/nao1215/truss/issues/386)). `format=gif` parses as a media type and has no encoder behind it, and the CLI and the Wasm package each caught that while reading the options while the server did not, so the server fetched the source, decoded the picture, and only then refused a request it was always going to refuse; for a `by-url` source that is an outbound fetch spent on nothing. The rule now lives in one place in the core rather than in a sentence written out four times, and all four adapters read it there. The server refuses the request with the same 415 and the same `unsupported-output-media-type` it sent before, so nothing about the answer changes except that it arrives before the source is read. A format name that is not a format, such as `bogus`, is still `invalid-request` and 400: not knowing a name and refusing a name are different failures.
- Two CLI exit codes that disagreed with the class of the failure they reported ([#383](https://github.com/nao1215/truss/issues/383)). Writing the class down per exit code is what made them visible: a decode failure out of the media sniff exited 3 while the same failure out of the transform exited 4, decided only by which of two call sites happened to raise it, and a failure inside `truss sign` exited 4, the transform code, for a signer that could not build a URL, which is not a transform at all. The sniff call sites in `convert` and `inspect` now report through the same mapping the transform does, so `decode-failed` is exit 4 and `unsupported-input-media-type` and `invalid-input` stay at exit 3 where they were; the signer's own failure exits 5, the runtime code. The `sign` path is not reachable from the command line today, because clap refuses a `--base-url` that is not http or https before the signer is called. Every other exit code is what it was.
- An AVIF whose primary item carries a clean aperture decodes, cut to the aperture ([#377](https://github.com/nao1215/truss/issues/377)). `clap` is the container-level crop MIAF defines alongside `irot` and `imir`, applied before either, and it must be marked essential; `mp4parse` does not read the box and forbids an item whose essential property it does not support, so `truss convert` failed with `AVIF has no primary item data` at exit 4, the server answered 422, and `truss inspect` reported the stored size rather than the cropped size a browser displays. The sniffer now reads the box's eight fields from the same walk that reads `irot` and `imir` and reports the aperture as the picture's size, and the decoder reads it from the same place, retries the container parse without the essential-property check when the strict parse dropped the item on that account, and cuts the decoded frame to the aperture before the orientation runs. The rectangle is worked in whole pixels the way ISO 14496-12 defines it, from the centre outward; an aperture that does not land on whole pixels or reaches outside the picture is refused with a message naming the field, because a viewer that rounds shows a different picture from one that does not. A file with no clean aperture keeps the strict parse and behaves as before. No issue is filed upstream; if a later `mp4parse` reads the box, the retry can go.
- `@nao1215/truss-wasm` no longer prints `using deprecated parameters for the initialization function` on every import ([#375](https://github.com/nao1215/truss/issues/375)). The package entry initializes the Wasm module at import time and handed the bytes or URL to the generated `init` as a bare argument, which wasm-bindgen has treated as the deprecated form since 0.2.93 and warns about on the console; every application that imported the package saw the line at startup, in Node and in the browser, and the next wasm-bindgen release that removes the old form would have broken initialization outright. The entry now passes `{ module_or_path }`, and the consumer smoke that installs the packed tarball into a throwaway Node project fails if importing it prints a deprecation warning, so the wrapper cannot regress silently. Nothing about the exported functions or their behaviour changes.

## v0.18.0

### Fixed

- A lossy encode that cannot reach the requested `targetQuality` now says so ([#370](https://github.com/nao1215/truss/issues/370)). The search binary-searches the encode quality for the lowest one whose score meets the target, and when no quality in its range does, it encoded at the top of the range and returned that as if it were an answer, at exit 0 and with nothing on stderr. The range is capped by `quality` when one is given, so `--quality 5 --target-quality psnr:40` returned a 24 dB file with the target dropped without a word, and a target no encoder reaches for a picture whose pixels changed did the same at quality 100. The shortfall is now a `TransformWarning::TargetQualityNotReached` carrying the target, the score the encode did reach, and the quality it reached it at, which the CLI prints as `warning:` on stderr, the server writes to its log, and the Wasm package returns in `warnings`, so the reader can tell a cap that is too low from a target that is too high. It is raised only for a target the caller named, not for the one `auto` aims at on its own, and not when the input's own bytes are handed back, since those score perfectly against themselves. The bytes returned are what they were before.

- `optimize: auto` no longer drops a named `targetQuality` when meeting it costs more bytes than the default encode ([#373](https://github.com/nao1215/truss/issues/373)). `auto` encodes a baseline at the default quality, runs the targeted search, and kept whichever was smaller; the baseline never looked at the target, so whenever the target needed a higher quality than the default the baseline won and `--optimize auto --target-quality psnr:45` returned the same bytes as no target at all, at exit 0 and with no warning, which the new shortfall warning could not catch because the search had succeeded and its result was then discarded. With a target the caller named, `auto` now returns the targeted encode: when the baseline would have met the target the search lands at or below its quality and is no larger, so the comparison only ever changed the answer when the baseline missed the target. The default target `auto` picks on its own is still weighed against the baseline as before, and the input's own bytes still pass through when they are smaller.
- `@nao1215/truss-wasm` accepts `stripMetadata`, the metadata key the HTTP server and the URL signer use ([#371](https://github.com/nao1215/truss/issues/371)). The Wasm options were the one place in the vocabulary that spelled the policy differently, as `keepMetadata`, and because unknown keys are refused, a transform options object built once for the server and reused for an in-browser preview failed on the preview with `unknown field stripMetadata`. The key now goes into the same `resolve_metadata_flags` every adapter uses, with the same precedence the server applies: `preserveExif` implies `stripMetadata: false` and overrides an explicit `true`, and `keepMetadata` together with `preserveExif` is still refused. `keepMetadata` remains and means `stripMetadata: false`, so no existing caller changes. A unit test now feeds every key the server accepts to the Wasm options, so the next spelling that drifts fails there.

### Breaking Changes

- `TransformWarning` no longer implements `Eq`. The new `TargetQualityNotReached` variant carries a `TargetQuality`, whose threshold is an `f32`, and `f32` has no total equality. `PartialEq` remains, so `assert_eq!` and `==` on warnings work as before; only a use that required `Eq`, such as a `HashSet<TransformWarning>`, needs to change.

## v0.17.0

### Fixed

- An AVIF input carrying an orientation is turned the way a browser turns it ([#359](https://github.com/nao1215/truss/issues/359)). AVIF has no EXIF orientation of its own kind: an encoder handed a phone photo with one writes the transform as the `irot` and `imir` item properties of the primary item, and Chrome, Firefox, and libheif apply those, so a photo that a browser showed upright came out of `truss convert photo.avif -o thumb.jpg --width 200` on its side, at exit 0 and with no warning, and `truss inspect` reported no `orientation` and stored dimensions for `orientedWidth` and `orientedHeight`. This was the one container the #357 change did not reach, because the tag was looked for as an Exif block and an AVIF does not carry it as one. The container walk the sniffer already does for the dimensions and the alpha item now also reads `pitm`, `ipma`, `irot`, and `imir`, folds the rotation and the mirror into the same eight orientation values under the MIAF rule that the rotation is applied before the mirror, and hands the result to the function every other container goes through, so the sniffer, the pipeline, the dropped-orientation warning, and the lossless-optimization refusal all see it. Properties associated with another item, such as an alpha plane's own rotation, are not read as the picture's. An Exif item inside the AVIF is still not read for orientation, which is what browsers do too: the properties are the signal the encoder chose, and honouring both would turn the picture twice. An AVIF with neither property behaves as it did.
- A 10-bit or 12-bit AVIF no longer decodes with its saturated samples wrapped to zero ([#362](https://github.com/nao1215/truss/issues/362)). The decoder narrows each deep sample to 8 bits by adding half a step and shifting, and a sample at the top of its range rounded past 255 and was cast to 0, so pure white came out black and a pure blue or red came out green, in `truss convert`, in the server, and in the Wasm package alike, at exit 0 and with no warning. High bit depth is the normal case for an HDR photo and for anything ImageMagick writes from a 16-bit source, and every test in the suite encoded its AVIF through the `image` crate, which writes 8-bit only, so the path was never exercised. The decoded planes were right all along; only the narrowing was wrong, and it now clamps at 255. The alpha plane goes through the same narrowing and gains the rounding the colour planes had. 8-bit AVIF decoded correctly before and is unchanged.
- The warning for an orientation dropped by `--no-auto-orient` fires whenever the tag does not reach the output, not only when the metadata policy said to strip it ([#361](https://github.com/nao1215/truss/issues/361)). The warning #331 added decided from the policy alone, before the input's metadata was read and before the output was encoded, so `--keep-metadata` silenced it for a TIFF input, whose metadata is never read, and for a BMP or TIFF output, which nothing writes an Exif block into; the picture came out on its side at exit 0 with nothing on stderr, which is the silent quarter turn the warning was written to name. An AVIF input is in the same position now that its orientation is read, since its properties are not metadata that can be carried into another format. The decision has moved to the end of the pipeline and asks the encoded bytes the question `inspect` asks of them: with auto-orientation off, an input tag from 2 to 8 that the output does not record is a drop, whatever the policy and whatever the formats. The lossless JPEG passthrough asks the same question of its own bytes, and the metadata it reports for the output carries the orientation read back from those bytes rather than copied from the input, so a strip that removed the segment no longer reports the tag it removed. JPEG, PNG, and WebP inputs going to JPEG, PNG, or WebP outputs with the metadata kept carry the tag through as before and do not warn.
- An SVG input rotates before it is resized, so `--rotate` together with `--width` and `--height` returns the size that was asked for ([#355](https://github.com/nao1215/truss/issues/355)). `docs/pipeline.md` and `truss help convert` both state that the stages run in the fixed order `auto-orient, rotate, crop, resize, ...` whatever order the options were written in, and the raster codec follows it; the SVG codec resized first and turned the finished canvas afterwards, so what came back was the rotated bounding box of the requested box. A 100x100 logo asked for at 200x100 with a quarter turn came back 100x200, where a PNG of the same picture came back 200x100, and a 200x200 thumbnail box with a 45-degree turn came back 283x283. A build step generating fixed-size thumbnails from a library holding both PNG and SVG logos therefore got one size for one half of the library and another size for the other half, reporting success on every file. The rotation now runs on the rasterized drawing before the resize, and the size the fit mode is asked about is the size of the rotated drawing, so `contain`, `cover`, `fill`, and `inside` all mean for an SVG what they mean for a raster source. The drawing is still rasterized at the scale the output needs rather than at its own size and scaled up: the rasterization size is mapped back through the rotation, which is exact for a quarter turn and, for any other angle, a uniform scale chosen so the rasterization is never coarser than the output, with `apply_resize` correcting the remaining pixel. A request with no rotation rasterizes and resizes exactly as before.
- EXIF orientation is honoured in every container that can carry it, not only in JPEG ([#357](https://github.com/nao1215/truss/issues/357)). PNG carries the tag in an `eXIf` chunk, WebP in an `EXIF` chunk, and TIFF in its first IFD, and Chrome, Firefox, Safari, and Pillow all apply it in those containers, so a photo that a browser showed upright came out of `truss convert photo.png -o thumb.png --width 200` turned by a quarter turn, at exit 0 and with no warning. The default flags were the bad case: `--auto-orient` is on by default and did nothing for those three formats, and the default `--strip-metadata` then removed the tag that was the only surviving record of the orientation, which is the silent rotation #331 added a warning for — except that warning was JPEG-gated too and did not fire. `truss inspect` showed the same gap from the other side, reporting no `orientation` and stored dimensions for `orientedWidth` and `orientedHeight`, so an application recording intrinsic dimensions once at upload time recorded the wrong ones for exactly the files where the fields were added to help. The tag is now located per container — a JPEG APP1 segment, a PNG `eXIf` chunk, a WebP `EXIF` chunk, a TIFF IFD entry — and decoded by the one reader that already existed, and the sniffers, the pipeline, the #331 warning, and the lossless-optimization passthrough all ask that one function, so none of them can drift from another. A retained tag is reset once the pixels have been turned, in every container rather than only in JPEG, so keeping the metadata no longer means the viewer turns them a second time. BMP and GIF cannot carry the tag. AVIF signals the transform as item properties rather than as a tag, and the entry for #359 covers it.
- `truss optimize --mode lossy` no longer returns more bytes than it was given ([#356](https://github.com/nao1215/truss/issues/356)). v0.16.0 made `auto` and `lossless` treat the input's own bytes as a candidate, but the gate listed those two modes by name, so `lossy` — the mode a caller reaches for when they want the largest reduction — was the one that could still grow a file. On an 800x600 flat-colour poster it returned 3.8 times the input and on 800x600 photographic noise 1.5 times, having paid a generation loss for the extra bytes; a script running it over an asset directory to shrink it produced a larger directory of visibly worse images and exited 0 on every file. `--target-quality` made it sharper, because the input satisfies any perceptual target trivially and at the smallest size: asking for `ssim:0.99` over the noise returned 296271 bytes where the input, which scores 1.0 against itself, is 166951. The mode check is now that the mode is not `none`, and the rest of the decision stays where it was — the same request predicate that already required the output format to equal the input format, every pixel-transforming option to be absent, and the metadata policy to be satisfied by the file as it stands. Naming a `--quality` still disqualifies the passthrough, because that is a request for a particular encode, so an unconditional re-encode remains one flag away. Where the encoder genuinely wins it still wins: a quality-85 gradient JPEG optimizes exactly as before.
- A watermark file that cannot be read is named in the error ([#358](https://github.com/nao1215/truss/issues/358)). Two paths go in on one command line and only the input was named when opening it failed, so `failed to read watermark file: No such file or directory` left a caller running a batch to work out which of the two was missing.
- `fit`, `position`, and `withoutEnlargement` now apply to SVG inputs ([#351](https://github.com/nao1215/truss/issues/351)). The rasterizer took the requested box as the render size and derived a scale for each axis from it, which is a non-uniform transform whenever the box and the drawing differ in aspect ratio — so every fit mode behaved as `fill`, and `contain`, the default nobody has to type, silently distorted the picture. A 100x100 logo asked for at 200x100 came back as an ellipse 180 pixels wide, where a PNG of the same dimensions and the same request came back circular and letterboxed; `inside` showed it in the output size alone, returning the full box instead of the aspect-preserving 50x30. SVG is the format logos, icons, and diagrams are stored in, and a fixed box is what a build step rasterizes them into, so this was the common request rather than an unusual one, and it reported success every time. The drawing is now rasterized at the size the fit mode scales the content to, at one uniform scale, and the padding `contain` adds and the crop `cover` takes are applied afterwards by the same `apply_resize` the raster codec uses, so both paths answer from one implementation and `position` and `withoutEnlargement` come along with it. The buffer `cover` materializes is larger than the box it returns, so it is now checked against the output pixel limit from dimensions alone before anything is allocated, which is the check `fit=cover` gained on the raster codec in v0.16.0. A request that names neither width nor height rasterizes at the document's own size exactly as before.
- `truss inspect` reports the dimensions an SVG declares ([#353](https://github.com/nao1215/truss/issues/353)). It was the only format for which `width`, `height`, `orientedWidth`, and `orientedHeight` were all `null`, including for a one-line document that names both in its root element, so an application inspecting an upload to record intrinsic dimensions, choose a thumbnail box, or reject an oversized asset had no answer for SVG and had to rasterize the file to get one — which is the work `inspect` exists to avoid. The numbers were never unavailable: `truss convert` needs them for every SVG it rasterizes and reads them from the parsed tree. They now come from the root element's `width` and `height` when those are absolute lengths, in `px`, `pt`, `pc`, `in`, `cm`, or `mm`, and from the `viewBox` extent otherwise, which is how SVG defines an intrinsic size; when one axis is absolute and the other is not, the missing one follows from the `viewBox` aspect ratio. A document that gives no absolute answer — a percentage with no `viewBox` behind it, a font-relative unit, nothing declared at all — still reports `null` rather than a guess, because resolving those needs a viewport that a file on disk does not carry. The scan reads one start tag rather than parsing the document, which matters because `sniff_artifact` runs on every server upload and runs before the sanitizer. `hasAlpha` stays `true` for SVG, which it always can be. The WASM `inspectImageJson` response carries the same values, since both adapters read one sniffer.
- Valid SVG files are no longer rejected as an unknown file signature because of their XML prolog ([#348](https://github.com/nao1215/truss/issues/348)). Detection walked a fixed sequence — declaration, then doctype, then comments — where XML defines the prolog as `XMLDecl? Misc* (doctypedecl Misc*)?` with comments and processing instructions legal on either side of the doctype and in any number. Three shapes fell outside that sequence: a comment before the doctype, a doctype carrying an internal subset — whose `>` characters ended the scan in the wrong place — and any processing instruction other than the declaration. The combination that fails is the one Adobe Illustrator writes on every export — declaration, generator comment, doctype with an internal subset declaring the Adobe namespace entities — so which editor produced a file decided whether truss would read it, and the error blamed the file. `sniff_artifact` is in `core.rs`, so this was the CLI exiting 3, the server answering 415, and WASM failing, with no flag to work around it. The prolog is now consumed as a loop over its legal items, and the doctype terminator is found past any internal subset and past `>` characters inside quoted identifiers.
- The SVG sanitizer removes external `url()` references from presentation attributes, not only from `style` ([#347](https://github.com/nao1215/truss/issues/347)). `style="filter:url(https://elsewhere/x.svg#f)"` had its reference emptied to `url()` and `filter="url(https://elsewhere/x.svg#f)"` was copied through untouched, so the answer depended on which of two spellings of one declaration the author picked. Every presentation attribute taking a `<funciri>` behaved the same way: `fill`, `stroke`, `filter`, `mask`, `clip-path`, the `marker-*` family, `cursor`. Where it reached a user is the SVG passthrough path — `truss convert --format svg`, and the server returning `image/svg+xml` — because a browser resolves the reference and fetches it, which turns a user-supplied image into a request every viewer makes to a host the uploader chose. Firefox resolves external `filter` references, and `Content-Security-Policy: sandbox` does not restrict subresource loads. Any attribute value containing `url(` now goes through the same rewrite the `style` attribute already did, so the decision comes from the value rather than from a list of attribute names and does not need revisiting when SVG grows another one. Internal references such as `fill="url(#gradient)"` are untouched, which is what these attributes are normally for. Event handler attributes are now matched on the namespace-stripped local name as well, the way the `href` and `style` rules already were, so `xlink:onload` is removed alongside `onload`; nothing executed under a prefix, but two notions of an attribute's name inside one function is how the next gap gets in.
- CSS escapes no longer defeat the removal of `@import` from sanitized SVG ([#349](https://github.com/nao1215/truss/issues/349)). The rule was found by searching the stylesheet for the literal string `@import`, and CSS identifiers admit escapes, so `@\69 mport "https://elsewhere/x.css";` and `@\import "https://elsewhere/x.css";` are the same at-rule to a renderer and matched nothing. The string form carries no `url()` either, so the separate pass over `url()` values did not catch it, and an external stylesheet loaded from a document the sanitizer had called safe. At-rules are now read with their escapes decoded and dropped whole unless they are on an allowlist — `charset`, `container`, `counter-style`, `font-face`, `font-feature-values`, `keyframes`, `layer`, `media`, `page`, `property`, `scope`, `starting-style`, `supports` — which removes the class rather than the spelling, the same move that removing the SMIL elements made for the attribute rules in v0.16.0. An at-rule not on that list is dropped along with its block or up to its semicolon; a `@` inside a quoted string is not an at-rule and is left alone.
- The SVG sanitizer removes processing instructions and refuses a doctype that declares external or nested entities. Both became reachable in the prolog with the fix above, and both defeat the rest of the sanitizer: `<?xml-stylesheet type="text/xsl" href="https://elsewhere/x.xsl"?>` is honoured by a browser rendering the document and an XSLT stylesheet generates arbitrary markup, while an internal subset declaring `<!ENTITY xxe SYSTEM "file:///etc/passwd">` is an XXE payload and one whose entities reference each other is the billion-laughs shape. Processing instructions are dropped and the XML declaration is kept, since it is a separate event and names nothing outside the document. A doctype is kept when its internal subset holds flat literal declarations, which is what editors write and what references to those entities need in order to resolve; a subset carrying `SYSTEM`, `PUBLIC`, or an `&` — one entity's replacement text referencing another — makes the document an error rather than a stripped one, because removing only the declarations would emit a document that is no longer well-formed.
- The server keeps accepting requests for the whole of the shutdown drain period ([#341](https://github.com/nao1215/truss/issues/341)). The accept loop used to leave as soon as SIGTERM arrived and only then sleep for `TRUSS_SHUTDOWN_DRAIN_SECS`, so for the whole of that window the listening socket was still open, the kernel completed the handshake for every new connection, and nothing ever accepted one. A client that connected in that window got no response, no error, and no close until the listener was dropped, and then an abrupt close with nothing written. That is the outage the drain exists to prevent: on a rolling deployment traffic keeps arriving for a moment after SIGTERM, and every request that landed there hung for the full drain and surfaced at the ingress as a 502. `docs/deployment.md` promised that `/health/ready` returns 503 during the period so that load balancers stop routing, and the readiness handler does have that branch, but a load balancer probes over a new connection and new connections were not being serviced — so the one signal the drain was waiting to send could not be sent, and following the documented advice to raise the drain or `terminationGracePeriodSeconds` only made the hang longer. The signal now sets a deadline instead of ending the loop, the wait on the listener is bounded while that deadline stands so an idle server still notices it, and the listener closes when it elapses — before the workers are drained, so the worker-drain window does not inherit the problem — which is what turns a connection attempted after the drain into an immediate refusal rather than another wait. The readiness 503 also carries `Retry-After` now, because a process that is going away is not one that is momentarily busy.
- A request with no `Accept` header no longer gets a cacheable image response without `Vary: Accept` ([#342](https://github.com/nao1215/truss/issues/342)). The header was emitted only when `Accept` had actually selected the format, so the same signed URL answered one client with `image/avif` and `Vary: Accept` and another with `image/png` and no `Vary` at all, both under `Cache-Control: public, max-age=3600`. Per RFC 9111 a stored response carrying no `Vary` matches every later request for that URI, so whichever client warmed a shared cache first pinned its representation on everyone behind that cache for the max-age — and a monitoring check, a deploy script, or a crawler is enough to be that client, which is the opposite of what a format-negotiating image server is for. `Vary` describes the resource rather than the request that happened to arrive: what matters is that `Accept` could have selected the representation, not that it did. The header now follows that predicate, which is the same condition negotiation is already gated on, so a URL carrying an explicit `format` still omits it and the cache-key narrowing from v0.16.0 is unaffected. The 406 returned for an `Accept` that names no supported type carries it too, for the same reason.
- `@nao1215/truss-wasm` no longer tells bundlers they may drop its Wasm initialization ([#345](https://github.com/nao1215/truss/issues/345)). The package declared `sideEffects: false` while its entry module's whole job is a top-level side effect: it runs `await init(...)` to instantiate the WebAssembly binary before re-exporting the functions that depend on it. That statement produces no value and touches nothing a bundler can see, so under that flag it is removable, and removing it leaves every export bound to a module that was never instantiated. Vite 8 removes it: the example in `examples/vite-truss-wasm`, built against Vite 8, compiles with no error and no warning, emits the `.wasm` as an asset that the bundle then references nowhere, and throws `Cannot read properties of undefined (reading '__wbindgen_add_to_stack_pointer')` on the first transform in the browser. Vite 7 happens to keep it, which is why this went unnoticed, but nothing in the package required it to. The flag now names the entry (`["./dist/truss.js"]`), which keeps the rest of the package freely tree-shakeable, and `scripts/run-wasm-consumer-smoke.mjs` asserts the entry is listed so the flag cannot be reset to `false` without a failing check. `@nao1215/truss-url-signer` keeps `sideEffects: false`, which is correct for it: pure functions, no top-level statements.
- Client-supplied `X-Request-Id` values are validated before being echoed ([#343](https://github.com/nao1215/truss/issues/343)). The header was reflected into the response after rejecting only CR, LF, and NUL, which stops a value from breaking out of its own line but leaves the rest of the field-value grammar unenforced: vertical tab, form feed, ESC, DEL, and raw 8-bit bytes all passed through, and there was no length limit at all. Any client could therefore make truss emit a header that RFC 9110 does not permit, which a reverse proxy in front of it is entitled to reject — turning a request into a 502 attributed to truss — and could attach thousands of bytes to every response and every access log line for that request. The value is now accepted only when it is printable ASCII and at most 128 characters, which covers UUIDs, `traceparent`, and every identifier a caller would realistically forward; anything else falls back to the generated UUID as an absent header always has.

### Changed

- Six settings the server reads from the environment are documented ([#344](https://github.com/nao1215/truss/issues/344)). `TRUSS_RATE_LIMIT_RPS`, `TRUSS_RATE_LIMIT_BURST`, `TRUSS_TRUSTED_PROXIES`, `TRUSS_CACHE_MAX_BYTES`, `TRUSS_FORMAT_PREFERENCE`, and `TRUSS_HEALTH_CACHE_TTL_SECS` appeared in no reference, no README, and no help output, so the per-IP rate limiter was a working feature nobody could find, and `TRUSS_CACHE_MAX_BYTES` defaulting to zero meant an operator who followed the documentation to enable the cache got one that grows until the disk fills, with the only setting that would have bounded it missing from the table they were reading. `TRUSS_TRUSTED_PROXIES` is the sharpest of the six, because it decides whether the rate limiter buckets by the real client IP or by the proxy's, and enabling the limit behind a CDN without it puts every client in one bucket. `docs/configuration.md` gains a row for each with its default and range, `docs/deployment.md` gains a section on rate limiting behind a proxy, and a test now enumerates the `TRUSS_*` names in `config.rs` and fails when one has no row, so the two sides cannot drift again.

### Breaking Changes

- An SVG-to-SVG request that also asks for a different picture is now refused ([#352](https://github.com/nao1215/truss/issues/352)). That path sanitizes the document and returns it as its author wrote it, so `width`, `height`, `rotate`, `grayscale`, and `background` could never have taken effect, and they were discarded in silence: `truss convert logo.svg -o small.svg --width 64` exited 0 and wrote the original at its original size, and a batch producing two sizes of an asset library produced two identical files and reported success on every one. The options next to them on the same path do report themselves — `blur`, `sharpen`, `crop`, and `watermark` are refused by the SVG codec, and `quality`, `optimize`, and `preserveExif` by `normalize` — so a caller had every reason to believe an accepted option had been applied. The check now lives beside those last three in `TransformOptions::normalize`, which is why the CLI (exit 1), the HTTP server (400 naming the parameter), and WASM all inherit it; `fit`, `position`, and `withoutEnlargement` need a width or a height and are refused through those. The message names a raster output format, since `truss convert logo.svg -o small.png --width 64` is the working request and the caller is one flag away from it. A sanitize request that passes no transform options is unaffected, which is what the SVG passthrough is for.

## v0.16.0

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
- Fixtures and end-to-end coverage for the above: `integration/fixtures` gains `exif-transposed-5.jpg`, `exif-transposed-7.jpg`, and `svg-animate-xss.svg`, and the atago suite gains scenarios pinning the cover buffer limit, the JPEG compositing equivalence between the direct and the padded paths, orientations 5 and 7 against baselines generated by an independent oracle, the sanitizer against the SMIL payload, the oriented dimensions reported by `inspect`, and the lowercase signature `truss sign` emits. The suite goes from 236 scenarios to 262.
- Every GitHub Release attaches `release-manifest.json`, a machine-readable index of the published binaries. A program that installs truss as a subprocess had to scrape the releases page for asset names and had nothing authoritative to check what it downloaded against: `checksums.txt` covered the archive but not the executable inside it, and nothing recorded which optional features a given binary carried. The manifest lists, per target, the Rust target triple, the OS, the architecture, the archive name, format, download URL, SHA-256 and byte size, the SHA-256 and byte size of the `truss` executable inside the archive, and the resolved Cargo feature set. It carries a `schemaVersion`, groups related values into nested objects so a field can be added without breaking a reader, and sorts its `artifacts` array by target triple so the same inputs always produce the same bytes. `checksums.txt` is now rendered from the manifest rather than accumulated from per-job sidecar files, so the two documents compute each archive hash once and cannot disagree; the Homebrew formula reads its URLs and hashes from the manifest for the same reason.
- Releases carry a CycloneDX 1.5 SBOM (`truss-<tag>-sbom.cdx.json`, generated by `cargo-cyclonedx` over all features and all targets) and a GitHub build provenance attestation over the binary archives, the manifest, and the SBOM.

### Changed

- Distribution archives are normalized. A `tar.gz` used to record the GitHub Actions runner's UID and GID on its single entry, which made extracting one as root fail on hosts without that user and leaked the build account into the artifact; the mode came from whatever the build left behind. Every archive now holds exactly one entry — the executable, at the archive root, no directory entry and no build-tree prefix — with mode 0755, uid 0, gid 0, empty owner and group names, and a fixed 2000-01-01T00:00:00Z timestamp, and gzip is told to store neither its own timestamp nor the input file name. Archives are therefore byte-identical for a given input binary. The binary itself is not reproducible — `cargo build --release` embeds paths and codegen ordering that vary per runner — and making it so is not part of this change; `scripts/pack-release-archive.sh` records why. Archive names, layout and the `bin.install "truss"` the Homebrew formula performs are unchanged, so existing install instructions keep working.
- The release workflow verifies every archive before anything is published. Each build job checks its own archive as soon as it is packed — it opens, holds the executable at the expected path with the expected mode and ownership — and runs `truss --version` against the tag on the targets the runner can execute natively; cross-compiled artifacts (`aarch64-unknown-linux-gnu`, and `x86_64-apple-darwin` on Apple Silicon runners) get every check except that one. A later job then regenerates nothing and checks the whole set at once: each archive's SHA-256 and size against the manifest, each extracted executable's SHA-256 and size against the manifest, `checksums.txt` against the manifest, that the manifest covers exactly the declared distribution targets, and that no target, archive name or download URL appears twice. The same verification runs a second time in the release job over the exact directory that is uploaded. crates.io, npm, GHCR and the GitHub Release all now depend on it passing.
- The release workflow builds with an explicit Rust version rather than `stable`, so the compiler that produced a published binary no longer depends on the day the tag was cut, and CI checks that pin against the crate's `rust-version` on every pull request. Third-party actions are pinned to full commit SHAs, the workflow's default permission is `contents: read` with jobs opting into more only where they publish, and the set of distribution targets lives in `.github/release-targets.json`, which supplies the build matrix and the manifest's coverage check from one place.

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
