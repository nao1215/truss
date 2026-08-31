# Problem types

Every error the HTTP server returns is an RFC 9457 problem details body, `application/problem+json`, with these members:

| Member | Meaning |
|---|---|
| `type` | A URI naming the class of failure. It is this page, at the anchor of one of the sections below, and it is the member to branch on: two responses with the same `type` failed for the same reason. `about:blank` means the status code is all there is to say. |
| `title` | The fixed short name of the class. It is the same for every occurrence of a `type` and never carries request specifics. |
| `status` | The HTTP status code, repeated in the body. |
| `detail` | What went wrong in this one request, for a person to read. Its wording is not part of the API; do not parse it. |
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

The transform classes carry the same names `@nao1215/truss-wasm` reports as `kind`, in kebab case, so a client that talks to the server and runs the package in the browser sees one classification. The CLI maps the same classes onto its exit codes: 1 for the request and the options, 3 for the input, 4 for the transform.

## Transform classes

### invalid-options

Status 400. The transform options are not a request the pipeline can carry out: a `fit` without both `width` and `height`, a `quality` on a lossless format, a `crop` outside the picture, an SVG passthrough that also asks for a different picture. The Wasm package reports it as `invalidOptions`; the CLI exits 1.

### invalid-input

Status 400. The input bytes were recognised but are not what was declared, such as a `declaredMediaType` that does not match the file's signature. Wasm `invalidInput`; CLI exit 3.

### decode-failed

Status 400. The input is a supported format but could not be decoded: a truncated file, a corrupt container, a clean aperture that does not fall on whole pixels. Wasm `decodeFailed`; CLI exit 4.

### unsupported-input-media-type

Status 415. The input is not a format the server decodes, or is one it decodes only in part, such as an animated GIF. Wasm `unsupportedInputMediaType`; CLI exit 3.

### unsupported-output-media-type

Status 415. The requested `format` is not one the server encodes, such as `gif`, or is not valid for this input, such as `svg` for a raster picture. Wasm `unsupportedOutputMediaType`; CLI exit 4.

### encode-failed

Status 500. The transform ran but the output could not be encoded. Wasm `encodeFailed`; CLI exit 4.

### capability-missing

Status 501. The request needs a codec or feature this build was compiled without, such as lossy WebP or AVIF. Wasm `capabilityMissing`; CLI exit 4.

### limit-exceeded

Status 413. The transform would exceed a pixel, size, or time limit of the pipeline: an output larger than the output pixel budget, a rotation whose canvas would be, a watermark that does not fit. Wasm `limitExceeded`; CLI exit 4. The server's own limits on what it accepts are `payload-too-large` and `unprocessable-entity`.

## Request classes

### invalid-request

Status 400. The request could not be understood before the transform was considered: a body that is not valid JSON, a missing required parameter, a query value that does not parse, a source of an unknown kind.

### unsupported-media-type

Status 415. The request's own media type, as opposed to the image's: a `Content-Type` other than `application/json` on the JSON endpoint, or a multipart part of the wrong kind.

### unauthorized

Status 401. The bearer token is missing or wrong, or the signed URL's signature, key id, or expiry does not verify. The bearer endpoints also send `WWW-Authenticate: Bearer`.

### forbidden

Status 403. The credentials verified but do not allow this request, such as a source outside the storage root or a URL source the server's policy refuses.

### not-found

Status 404. The source the request named does not exist. A route that does not exist also answers 404, with `type` `about:blank`.

### not-acceptable

Status 406. The `Accept` header allows none of the output formats the server can produce.

### request-timeout

Status 408. The client stopped sending before the request line, headers, or body were complete.

### payload-too-large

Status 413. The request, its headers, its body, or the source it named is larger than the server accepts (`TRUSS_MAX_UPLOAD_BYTES`, `TRUSS_MAX_SOURCE_BYTES`, and the header limits).

### unprocessable-entity

Status 422. The input was read but exceeds what the server will process, such as `TRUSS_MAX_INPUT_PIXELS`.

### too-many-requests

Status 429. The rate limit for the client's address was reached. `Retry-After` says when to try again.

## Server classes

### internal-error

Status 500. The server failed in a way that is not the request's doing, such as a source it could not read.

### not-implemented

Status 501. The request names something the server knows of but does not do, such as a storage backend this binary was built without.

### bad-gateway

Status 502. A remote source or storage backend answered with an error, timed out, or could not be reached, including an upstream 401 that means the server's own credentials are wrong.

### service-unavailable

Status 503. The server is draining for shutdown, has no free transform slot, or failed its readiness check.

### loop-detected

Status 508. A remote source redirected more times than `TRUSS_MAX_REMOTE_REDIRECTS` allows.
