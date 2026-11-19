# Testing

Each technique here is used because it catches a kind of bug the others miss. This page says what each one runs on, where its tests live, and how CI runs it. The commands for the checks a pull request needs are in [CONTRIBUTING.md](../CONTRIBUTING.md).

## Unit and integration tests

The unit tests sit beside the code they test, in a `#[cfg(test)] mod tests` at the end of each file, and cover the arithmetic, validation and parsing that need no process or file. Sets that have to be covered exhaustively, such as the eight EXIF orientations or the four fit modes, are `rstest` tables. The tests under `tests/` start the CLI as a process or the HTTP server on a socket, one file per surface. CI runs both with `cargo nextest run --all-features --all-targets` on Linux, macOS and Windows.

## CLI end-to-end tests

`e2e/atago/` holds [atago](https://github.com/nao1215/atago) specs that run the released shape of the binary: `e2e/run.sh` builds `truss` in release mode and runs every spec against it. Outputs whose exact bytes are the contract, such as what `truss optimize` writes, are atago snapshots in `e2e/atago/testdata/*.snap`. The `CLI Integration` workflow runs the suite on Linux, macOS and Windows.

## Property-based tests

[proptest](https://github.com/proptest-rs/proptest) generates the inputs and checks a property every answer has to satisfy, so a size that is one pixel off for one aspect ratio in a thousand is found without anyone writing that case down.

- `mod properties` at the end of [src/codecs/raster.rs](../src/codecs/raster.rs) covers the resize and rotation arithmetic: `contain`, `cover` and `fill` return the box, `inside` stays within it and keeps the aspect ratio to the half pixel, `--without-enlargement` never grows the content and changes nothing for a shrink, the source size is a fixed point, and the rotated canvas holds the whole image.
- `mod properties` at the end of [src/core.rs](../src/core.rs) covers the value types every adapter prints and parses: rotations wrap whole turns, and crop regions, rotations and colours parse what they print.

## Metamorphic tests

[tests/transform_metamorphic.rs](../tests/transform_metamorphic.rs) runs the public `transform` on generated images and checks how two runs relate, which is something that can be stated exactly when the right output of one run cannot. Four quarter turns and two half turns are no turn, a turn and its opposite cancel, a full crop is the identity, a crop takes exactly its rectangle, rotating then cropping gives the same pixels as one request or two and as cropping first with the rectangle mapped back, a round trip through WebP, BMP or TIFF keeps the pixels, a resize that changes no size keeps them, and grayscale is idempotent.

Both property and metamorphic tests run in every `cargo test` with proptest's default of 256 cases each and a fresh seed each run; a failure prints the minimal input it shrank to. The `Nightly properties` workflow runs them in release mode with 20,000 cases each. To run them that way locally:

```sh
PROPTEST_CASES=20000 cargo test --release --lib properties
PROPTEST_CASES=20000 cargo test --release --test transform_metamorphic
```

A failure becomes a plain unit test with the input proptest printed, fixed on its own branch.

## Fuzzing

`fuzz/` holds cargo-fuzz targets for the code that reads untrusted input: the sniffer, the transform pipeline, SVG sanitization, signed URLs, raw HTTP requests and multipart uploads. The `Fuzz` workflow runs each target for five minutes every night; [CONTRIBUTING.md](../CONTRIBUTING.md#fuzzing) has the local commands.

## Benchmarks

`bench/himorime.yaml` measures the CLI end to end with [himorime](https://github.com/nao1215/himorime), and the `Benchmark` workflow compares a pull request with its base in the same job. `benches/transform.rs` is a criterion suite for the library's pixel stages. [docs/development.md](development.md#benchmark) has the commands.
