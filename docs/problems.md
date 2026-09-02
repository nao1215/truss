# Problem types

Every error the HTTP server returns is an RFC 9457 problem details body, `application/problem+json`, with these members:

| Member | Meaning |
|---|---|
| `type` | A URI naming the class of failure. It is this page, at the anchor of one of the sections below, and it is the member to branch on: two responses with the same `type` failed for the same reason. `about:blank` means the status code is all there is to say. |
| `title` | The fixed short name of the class. It is the same for every occurrence of a `type` and never carries request specifics. |
| `status` | The HTTP status code, repeated in the body. |
| `detail` | What went wrong in this one request, for a person to read. It is one line: a message that came from a decoder and holds a line break is folded onto one before it is sent, the same way the CLI prints one. Its wording is not part of the API; do not parse it. |
| `requestId` | The request id, the same value as the `X-Request-Id` response header and the `request_id` field of the server's access log, so a body kept on its own can still be matched to the log line. |

```json
{
  "type": "https://github.com/nao1215/truss/blob/main/docs/problems.md#invalid-options",
  "title": "Invalid transform options",
  "status": 400,
  "detail": "fit requires both width and height",
  "requestId": "6b3d0a2c-2f6a-4c53-9a1f-0f4b2c1e8d7a"
}
```

## One class, three spellings

The class is the same failure whichever adapter reports it, so the three adapters spell one name three ways and truss reads all three from one table. The server puts the slug in `type`. `@nao1215/truss-wasm` reports the slug in camel case as `kind`. The CLI prints the slug in parentheses after its message, `error: fit requires both width and height (invalid-options)`, and exits with the code below.

A dash means the adapter cannot reach the class: the CLI has no `Accept` header to refuse and no rate limit, and the Wasm adapter runs no server, so only the transform classes reach the browser.

| Class | CLI exit code | HTTP status | Wasm `kind` |
|---|---|---|---|
| [invalid-options](#invalid-options) | 1 | 400 | `invalidOptions` |
| [invalid-input](#invalid-input) | 3 | 400 | `invalidInput` |
| [decode-failed](#decode-failed) | 4 | 400 | `decodeFailed` |
| [unsupported-input-media-type](#unsupported-input-media-type) | 3 | 415 | `unsupportedInputMediaType` |
| [unsupported-output-media-type](#unsupported-output-media-type) | 4 | 415 | `unsupportedOutputMediaType` |
| [encode-failed](#encode-failed) | 4 | 500 | `encodeFailed` |
| [capability-missing](#capability-missing) | 4 | 501 | `capabilityMissing` |
| [limit-exceeded](#limit-exceeded) | 4 | 413 | `limitExceeded` |
| [invalid-request](#invalid-request) | 1 | 400 | — |
| [unsupported-media-type](#unsupported-media-type) | — | 415 | — |
| [unauthorized](#unauthorized) | — | 401 | — |
| [forbidden](#forbidden) | — | 403 | — |
| [not-found](#not-found) | 2 | 404 | — |
| [not-acceptable](#not-acceptable) | — | 406 | — |
| [request-timeout](#request-timeout) | — | 408 | — |
| [payload-too-large](#payload-too-large) | — | 413 | — |
| [unprocessable-entity](#unprocessable-entity) | — | 422 | — |
| [too-many-requests](#too-many-requests) | — | 429 | — |
| [internal-error](#internal-error) | 2 or 5 | 500 | — |
| [not-implemented](#not-implemented) | — | 501 | — |
| [bad-gateway](#bad-gateway) | 2 | 502 | — |
| [service-unavailable](#service-unavailable) | — | 503 | — |
| [loop-detected](#loop-detected) | — | 508 | — |

The CLI names the class on stderr as `error: <message> (<class>)`, always on one line, so the class is the last thing on the line whatever the message came from. The CLI's five exit codes are coarser than the classes, so a class determines an exit code but an exit code does not determine a class. `internal-error` is the one class on two of them: the CLI separates a fault while reading or writing (2) from one after that (5), and both are the same class to the server.

## Transform classes

### invalid-options

The transform options are not a request the pipeline can carry out: a `fit` without both `width` and `height`, a `quality` on a lossless format, a `crop` outside the picture, an SVG passthrough that also asks for a different picture.

### invalid-input

The input bytes were recognised but are not what was declared, such as a `declaredMediaType` that does not match the file's signature.

### decode-failed

The input is a supported format but could not be decoded: a truncated file, a corrupt container, a clean aperture that does not fall on whole pixels. The CLI reports it as a transform failure wherever it is raised, including the media sniff that runs before the transform.

### unsupported-input-media-type

The input is not a format truss decodes, or is one it decodes only in part, such as an animated GIF.

### unsupported-output-media-type

The requested `format` is not one truss encodes, such as `gif`, or is not valid for this input, such as `svg` for a raster picture. A format truss reads but cannot write is refused from the options, before the source is read; a format that depends on the input, such as `svg`, is refused by the transform. On the CLI a `--format` value truss cannot write is refused by the command line parser instead, which is a usage error and exit 1; the transform-time refusal is exit 4.

### encode-failed

The transform ran but the output could not be encoded.

### capability-missing

The request needs a codec or feature this build was compiled without, such as lossy WebP or AVIF.

### limit-exceeded

The transform would exceed a pixel, size, or time limit of the pipeline: an output larger than the output pixel budget, a rotation whose canvas would be, a watermark that does not fit. The server's own limits on what it accepts are `payload-too-large` and `unprocessable-entity`.

## Request classes

### invalid-request

The request could not be understood before the transform was considered: a body that is not valid JSON, a missing required parameter, a query value that does not parse, a source of an unknown kind. On the CLI it is the command line that could not be understood: an unknown subcommand, a missing flag, a value clap refuses.

### unsupported-media-type

The request's own media type, as opposed to the image's: a `Content-Type` other than `application/json` on the JSON endpoint, or a multipart part of the wrong kind.

### unauthorized

The bearer token is missing or wrong, or the signed URL's signature, key id, or expiry does not verify. The bearer endpoints also send `WWW-Authenticate: Bearer`.

### forbidden

The credentials verified but do not allow this request, such as a source outside the storage root or a URL source the server's policy refuses.

### not-found

The source the request named does not exist, which on the CLI is any file the command line named that is not there: the input, or a `--watermark`. A route that does not exist also answers 404, with `type` `about:blank`.

### not-acceptable

The `Accept` header allows none of the output formats the server can produce.

### request-timeout

The client stopped sending before the request line, headers, or body were complete.

### payload-too-large

The request, its headers, its body, or the source it named is larger than the server accepts (`TRUSS_MAX_UPLOAD_BYTES`, `TRUSS_MAX_SOURCE_BYTES`, and the header limits).

### unprocessable-entity

The input was read but exceeds what the server will process, such as `TRUSS_MAX_INPUT_PIXELS`.

### too-many-requests

The rate limit for the client's address was reached. `Retry-After` says when to try again.

## Server classes

### internal-error

truss failed in a way that is not the request's doing, such as a source it could not read. On the CLI this is every I/O fault other than a file that is not there or a refused fetch (exit 2), such as a source that cannot be read or an output that cannot be written, and every fault after the input is read: a port already in use, a standard output that cannot be written (exit 5).

### not-implemented

The request names something the server knows of but does not do, such as a storage backend this binary was built without.

### bad-gateway

A remote source or storage backend answered with an error, timed out, or could not be reached, including an upstream 401 that means the server's own credentials are wrong. On the CLI it is a `--url` input the remote end did not deliver.

### service-unavailable

The server is draining for shutdown, has no free transform slot, or is missing a configuration a route requires. `Retry-After` says when to try again where waiting is what resolves the condition, which covers a request shed for want of a transform slot and a probe answered while the process is draining; it is absent when the cause is a configuration, since waiting does not change one. A request whose answer is already in the cache is served whether or not the slots are free, because it needs none.

### loop-detected

A remote source redirected more times than `TRUSS_MAX_REMOTE_REDIRECTS` allows.
