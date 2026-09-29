# Benchmarks

The truss CLI is measured the way a user runs it, one process per call from start to exit, with [himorime](https://github.com/nao1215/himorime). himorime builds truss in release mode, runs each command in interleaved rounds, and reports latency, CPU time and peak RSS.

```console
$ go install github.com/nao1215/himorime@latest
$ just bench-cli                                # himorime run bench: measure the working tree
$ just bench-cli-compare                        # himorime compare --against main bench
$ BASE=v0.26.1 just bench-cli-compare           # compare another revision with the working tree
$ himorime run --filter '^convert 4000x3000$' bench  # one benchmark
```

The input pictures are not committed. `gen.sh` writes them into each benchmark's working directory from a fixed seed (`sh gen.sh png <width>x<height> <out>`, Python 3 standard library only): a gradient with fine noise, so it compresses and resizes like a photograph. The suite calls it through `${head_root}`, so the base and the head of a comparison read the same bytes. No benchmark touches the network.

On a pull request, `.github/workflows/bench.yml` runs `himorime ci bench`: the base of the pull request and its head are built and measured in the same rounds on one runner. The job fails when a command is slower, uses more CPU time or more memory than the base beyond the tolerance in `himorime.yaml` with 95% confidence. A difference too close to call is reported as inconclusive and does not fail the job.

| Benchmark | Commands | Measures |
|-----------|----------|----------|
| `version` | `truss --version` | starting truss |
| `convert 640x480` | `truss convert small.png -o out.jpg --width 320 --quality 80`, and to `out.webp` | a web-sized picture resized and re-encoded, where start-up and I/O weigh as much as the pixels |
| `convert 4000x3000` | `truss convert large.png -o out.jpg --width 1600 --quality 80`, and to `out.webp` | a 12-megapixel, 34 MB PNG decoded, resized and re-encoded; also reported as throughput of the input |
| `convert error` | `truss convert missing.png -o out.webp` | the failure path of a missing input, exit code 2 |
| `inspect 4000x3000` | `truss inspect large.png` | reading the format and size of the 12-megapixel PNG without decoding it |

Not measured:

| Command | Why |
|---------|-----|
| `truss serve` | the server does not exit, and himorime measures a process from start to exit. The HTTP API is covered by the runn tests in `integration/` |
| `truss convert --url` | it fetches over the network; a local stand-in server would measure the same decode and encode as `convert 4000x3000` |

## Two kinds of benchmark

`benches/transform.rs` stays: it is a criterion benchmark of the library's pixel path (decode, resize, encode inside one process), which is what a change to `src/codecs` or the transform pipeline shows first, and what the library and WASM users call. This suite measures the `truss` binary a user runs. That is why `just bench` still runs criterion and the himorime recipes are `just bench-cli` and `just bench-cli-compare` instead of the `bench` and `bench-compare` names the other nao1215 CLIs use.

Numbers from different machines are not comparable; compare revisions on one machine, as `himorime compare` and CI do.
