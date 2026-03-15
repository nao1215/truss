import { createHmac } from "node:crypto";

const FIT_MODES = new Set(["contain", "cover", "fill", "inside"]);
const POSITIONS = new Set([
  "center",
  "top",
  "right",
  "bottom",
  "left",
  "top-left",
  "top-right",
  "bottom-left",
  "bottom-right",
]);
const OUTPUT_FORMATS = new Set(["jpeg", "png", "webp", "avif", "svg", "bmp", "tiff"]);
const OPTIMIZE_MODES = new Set(["none", "auto", "lossless", "lossy"]);
const LOSSY_FORMATS = new Set(["jpeg", "webp", "avif"]);
const OPTIMIZABLE_FORMATS = new Set(["jpeg", "png", "webp", "avif"]);
const QUARTER_TURNS = new Set([0, 90, 180, 270]);
const HEX_COLOR_PATTERN = /^[0-9A-Fa-f]{6}([0-9A-Fa-f]{2})?$/;

export function signPublicUrl(options) {
  if (!isObject(options)) {
    throw new TypeError("signPublicUrl options must be an object");
  }

  const source = normalizeSource(options.source);
  const transforms = normalizeTransforms(options.transforms);
  const watermark = normalizeWatermark(options.watermark);
  const endpoint = resolveEndpoint(options.baseUrl, source);
  const queryEntries = [];

  appendSourceEntries(queryEntries, source);
  appendTransformEntries(queryEntries, transforms);
  appendWatermarkEntries(queryEntries, watermark);

  if (options.preset !== undefined) {
    pushNonEmptyString(queryEntries, "preset", options.preset);
  }

  pushNonEmptyString(queryEntries, "keyId", options.keyId);
  queryEntries.push(["expires", normalizeExpires(options.expires)]);

  const canonical = [
    normalizeMethod(options.method),
    urlAuthority(endpoint),
    endpoint.pathname,
    encodeSortedQuery(queryEntries),
  ].join("\n");

  const signature = createHmac("sha256", normalizeSecret(options.secret))
    .update(canonical, "utf8")
    .digest("hex");

  endpoint.search = encodeSortedQuery([...queryEntries, ["signature", signature]]);
  return endpoint.toString();
}

function resolveEndpoint(baseUrl, source) {
  return new URL(routePathForSource(source), normalizeBaseUrl(baseUrl));
}

function normalizeBaseUrl(baseUrl) {
  let url;

  try {
    url = baseUrl instanceof URL ? new URL(baseUrl.toString()) : new URL(String(baseUrl));
  } catch (error) {
    throw new TypeError(`base URL is invalid: ${error.message}`);
  }

  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new TypeError("base URL must use the http or https scheme");
  }

  return url;
}

function routePathForSource(source) {
  switch (source.kind) {
    case "path":
      return "/images/by-path";
    case "url":
      return "/images/by-url";
    default:
      throw new TypeError("source.kind must be either `path` or `url`");
  }
}

function appendSourceEntries(entries, source) {
  switch (source.kind) {
    case "path":
      entries.push(["path", source.path]);
      pushOptionalString(entries, "version", source.version);
      break;
    case "url":
      entries.push(["url", source.url]);
      pushOptionalString(entries, "version", source.version);
      break;
    default:
      throw new TypeError("source.kind must be either `path` or `url`");
  }
}

function appendTransformEntries(entries, transforms) {
  if (transforms === null) {
    return;
  }

  pushOptionalInteger(entries, "width", transforms.width);
  pushOptionalInteger(entries, "height", transforms.height);
  pushOptionalString(entries, "fit", transforms.fit);
  pushOptionalString(entries, "position", transforms.position);
  pushOptionalString(entries, "format", transforms.format);
  pushOptionalInteger(entries, "quality", transforms.quality);

  if (transforms.optimize !== null && transforms.optimize !== "none") {
    entries.push(["optimize", transforms.optimize]);
  }

  pushOptionalString(entries, "targetQuality", transforms.targetQuality);
  pushOptionalString(entries, "background", transforms.background);

  if (transforms.rotate !== null && transforms.rotate !== 0) {
    entries.push(["rotate", String(transforms.rotate)]);
  }

  if (transforms.autoOrient === false) {
    entries.push(["autoOrient", "false"]);
  }
  if (transforms.stripMetadata === false) {
    entries.push(["stripMetadata", "false"]);
  }
  if (transforms.preserveExif === true) {
    entries.push(["preserveExif", "true"]);
  }

  pushOptionalString(entries, "crop", transforms.crop);
  pushOptionalNumber(entries, "blur", transforms.blur);
  pushOptionalNumber(entries, "sharpen", transforms.sharpen);
}

function appendWatermarkEntries(entries, watermark) {
  if (watermark === null) {
    return;
  }

  entries.push(["watermarkUrl", watermark.url]);
  pushOptionalString(entries, "watermarkPosition", watermark.position);
  pushOptionalInteger(entries, "watermarkOpacity", watermark.opacity);
  pushOptionalInteger(entries, "watermarkMargin", watermark.margin);
}

function normalizeMethod(method) {
  if (method === undefined) {
    return "GET";
  }

  if (typeof method !== "string" || method.length === 0) {
    throw new TypeError("method must be a non-empty string when provided");
  }

  return method.toUpperCase();
}

function normalizeExpires(expires) {
  if (typeof expires === "bigint") {
    if (expires < 0n) {
      throw new TypeError("expires must be a non-negative integer");
    }
    return expires.toString();
  }

  if (!Number.isSafeInteger(expires) || expires < 0) {
    throw new TypeError("expires must be a non-negative safe integer or bigint");
  }

  return String(expires);
}

function normalizeSecret(secret) {
  if (secret === undefined || secret === null) {
    throw new TypeError("secret must be provided");
  }

  return secret;
}

function normalizeSource(source) {
  if (!isObject(source)) {
    throw new TypeError("source must be an object");
  }

  switch (source.kind) {
    case "path":
      return {
        kind: "path",
        path: normalizeRequiredString("path", source.path),
        version: normalizeOptionalString("version", source.version),
      };
    case "url":
      return {
        kind: "url",
        url: normalizeRemoteUrl("url", source.url),
        version: normalizeOptionalString("version", source.version),
      };
    default:
      throw new TypeError("source.kind must be either `path` or `url`");
  }
}

function normalizeTransforms(transforms) {
  if (transforms === undefined) {
    return null;
  }

  if (!isObject(transforms)) {
    throw new TypeError("transforms must be an object when provided");
  }

  const width = normalizeOptionalPositiveInteger("width", transforms.width);
  const height = normalizeOptionalPositiveInteger("height", transforms.height);
  const fit = normalizeOptionalEnum("fit", transforms.fit, FIT_MODES, "unsupported fit mode");
  const position = normalizeOptionalEnum(
    "position",
    transforms.position,
    POSITIONS,
    "unsupported position",
  );
  const format = normalizeOptionalEnum(
    "format",
    transforms.format,
    OUTPUT_FORMATS,
    "unsupported media type",
  );
  const quality = normalizeOptionalBoundedInteger(
    "quality",
    transforms.quality,
    1,
    100,
    "quality must be between 1 and 100",
  );
  const optimize =
    normalizeOptionalEnum(
      "optimize",
      transforms.optimize,
      OPTIMIZE_MODES,
      "unsupported optimize mode",
    ) ?? null;
  const targetQuality = normalizeOptionalTargetQuality(transforms.targetQuality);
  const background = normalizeOptionalBackground(transforms.background);
  const rotate = normalizeOptionalRotation(transforms.rotate);
  const autoOrient = normalizeOptionalBoolean("autoOrient", transforms.autoOrient);
  const stripMetadata = normalizeOptionalBoolean(
    "stripMetadata",
    transforms.stripMetadata,
  );
  const preserveExif = normalizeOptionalBoolean(
    "preserveExif",
    transforms.preserveExif,
  );
  const crop = normalizeOptionalCrop(transforms.crop);
  const blur = normalizeOptionalSigma("blur", transforms.blur);
  const sharpen = normalizeOptionalSigma("sharpen", transforms.sharpen);

  validateTransformMatrix({
    width,
    height,
    fit,
    position,
    format,
    quality,
    optimize,
    targetQuality,
    stripMetadata,
    preserveExif,
  });

  return {
    width,
    height,
    fit,
    position,
    format,
    quality,
    optimize,
    targetQuality,
    background,
    rotate,
    autoOrient,
    stripMetadata,
    preserveExif,
    crop,
    blur,
    sharpen,
  };
}

function normalizeWatermark(watermark) {
  if (watermark === undefined) {
    return null;
  }

  if (!isObject(watermark)) {
    throw new TypeError("watermark must be an object when provided");
  }

  return {
    url: normalizeRemoteUrl("watermarkUrl", watermark.url),
    position: normalizeOptionalEnum(
      "watermarkPosition",
      watermark.position,
      POSITIONS,
      "unsupported position",
    ),
    opacity: normalizeOptionalBoundedInteger(
      "watermarkOpacity",
      watermark.opacity,
      1,
      100,
      "watermarkOpacity must be between 1 and 100",
    ),
    margin: normalizeOptionalNonNegativeInteger(
      "watermarkMargin",
      watermark.margin,
    ),
  };
}

function validateTransformMatrix(transforms) {
  const hasBoundedResize =
    transforms.width !== undefined && transforms.height !== undefined;

  if (transforms.fit !== undefined && !hasBoundedResize) {
    throw new TypeError("fit requires both width and height");
  }

  if (transforms.position !== undefined && !hasBoundedResize) {
    throw new TypeError("position requires both width and height");
  }

  if (transforms.preserveExif === true && transforms.stripMetadata !== false) {
    throw new TypeError("preserveExif requires stripMetadata to be false");
  }

  if (
    transforms.optimize !== null &&
    transforms.optimize !== "none" &&
    transforms.format !== undefined &&
    !OPTIMIZABLE_FORMATS.has(transforms.format)
  ) {
    throw new TypeError(
      `optimization is not supported for ${transforms.format} output`,
    );
  }

  if (
    transforms.optimize === "lossy" &&
    transforms.format !== undefined &&
    !LOSSY_FORMATS.has(transforms.format)
  ) {
    throw new TypeError(
      `lossy optimization requires jpeg, webp, or avif output, got ${transforms.format}`,
    );
  }

  if (transforms.preserveExif === true && transforms.format === "svg") {
    throw new TypeError("preserveExif is not supported with SVG output");
  }

  if (
    transforms.quality !== undefined &&
    transforms.format !== undefined &&
    !LOSSY_FORMATS.has(transforms.format)
  ) {
    throw new TypeError("quality requires a lossy output format");
  }

  if (transforms.quality !== undefined && transforms.optimize === "lossless") {
    throw new TypeError("quality cannot be combined with optimize=lossless");
  }

  if (
    transforms.targetQuality !== undefined &&
    (transforms.optimize === null ||
      transforms.optimize === "none" ||
      transforms.optimize === "lossless")
  ) {
    throw new TypeError("targetQuality requires optimize=auto or optimize=lossy");
  }

  if (
    transforms.targetQuality !== undefined &&
    transforms.format !== undefined &&
    !LOSSY_FORMATS.has(transforms.format)
  ) {
    throw new TypeError("targetQuality requires jpeg, webp, or avif output");
  }
}

function urlAuthority(url) {
  return url.port === "" ? url.hostname : `${url.hostname}:${url.port}`;
}

function encodeSortedQuery(entries) {
  const params = new URLSearchParams();

  for (const [name, value] of [...entries].sort(compareQueryNames)) {
    params.append(name, value);
  }

  return params.toString();
}

function compareQueryNames([left], [right]) {
  if (left < right) {
    return -1;
  }
  if (left > right) {
    return 1;
  }
  return 0;
}

function pushOptionalString(entries, name, value) {
  if (value !== undefined) {
    entries.push([name, value]);
  }
}

function pushOptionalInteger(entries, name, value) {
  if (value !== undefined) {
    entries.push([name, String(value)]);
  }
}

function pushOptionalNumber(entries, name, value) {
  if (value !== undefined) {
    entries.push([name, String(value)]);
  }
}

function pushNonEmptyString(entries, name, value) {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${name} must be a non-empty string`);
  }
  entries.push([name, value]);
}

function normalizeOptionalString(name, value) {
  if (value === undefined) {
    return undefined;
  }
  return normalizeRequiredString(name, value);
}

function normalizeRequiredString(name, value) {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${name} must be a non-empty string`);
  }
  return value;
}

function normalizeOptionalBoolean(name, value) {
  if (value === undefined) {
    return undefined;
  }
  if (typeof value !== "boolean") {
    throw new TypeError(`${name} must be a boolean`);
  }
  return value;
}

function normalizeOptionalPositiveInteger(name, value) {
  const normalized = normalizeOptionalInteger(name, value);
  if (normalized !== undefined && normalized <= 0) {
    throw new TypeError(`${name} must be greater than zero`);
  }
  return normalized;
}

function normalizeOptionalNonNegativeInteger(name, value) {
  const normalized = normalizeOptionalInteger(name, value);
  if (normalized !== undefined && normalized < 0) {
    throw new TypeError(`${name} must be a non-negative integer`);
  }
  return normalized;
}

function normalizeOptionalBoundedInteger(name, value, min, max, message) {
  const normalized = normalizeOptionalInteger(name, value);
  if (
    normalized !== undefined &&
    (normalized < min || normalized > max)
  ) {
    throw new TypeError(message);
  }
  return normalized;
}

function normalizeOptionalInteger(name, value) {
  if (value === undefined) {
    return undefined;
  }
  if (!Number.isSafeInteger(value)) {
    throw new TypeError(`${name} must be a finite integer`);
  }
  return value;
}

function normalizeOptionalNumber(name, value) {
  if (value === undefined) {
    return undefined;
  }
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new TypeError(`${name} must be a finite number`);
  }
  return value;
}

function normalizeOptionalSigma(name, value) {
  const normalized = normalizeOptionalNumber(name, value);
  if (
    normalized !== undefined &&
    (normalized < 0.1 || normalized > 100.0)
  ) {
    throw new TypeError(`${name} sigma must be between 0.1 and 100.0`);
  }
  return normalized;
}

function normalizeOptionalRotation(value) {
  if (value === undefined) {
    return null;
  }
  const normalized = normalizeOptionalInteger("rotate", value);
  if (!QUARTER_TURNS.has(normalized)) {
    throw new TypeError("rotate must be 0, 90, 180, or 270");
  }
  return normalized;
}

function normalizeOptionalEnum(name, value, allowed, label) {
  if (value === undefined) {
    return undefined;
  }
  const normalized = normalizeRequiredString(name, value);
  if (!allowed.has(normalized)) {
    throw new TypeError(`${label} \`${normalized}\``);
  }
  return normalized;
}

function normalizeOptionalBackground(value) {
  if (value === undefined) {
    return undefined;
  }
  const normalized = normalizeRequiredString("background", value);
  if (!HEX_COLOR_PATTERN.test(normalized)) {
    throw new TypeError(`unsupported color \`${normalized}\``);
  }
  return normalized.toUpperCase();
}

function normalizeOptionalCrop(value) {
  if (value === undefined) {
    return undefined;
  }
  const normalized = normalizeRequiredString("crop", value);
  const parts = normalized.split(",");

  if (parts.length !== 4) {
    throw new TypeError(
      `crop must be x,y,w,h (four comma-separated integers), got '${normalized}'`,
    );
  }

  const [x, y, width, height] = parts;
  assertCropInteger("x", x);
  assertCropInteger("y", y);
  assertCropInteger("width", width);
  assertCropInteger("height", height);

  if (width === "0" || height === "0") {
    throw new TypeError("crop width and height must be greater than zero");
  }

  return normalized;
}

function assertCropInteger(name, value) {
  if (!/^\d+$/.test(value)) {
    throw new TypeError(`crop ${name} must be a non-negative integer, got '${value}'`);
  }
}

function normalizeOptionalTargetQuality(value) {
  if (value === undefined) {
    return undefined;
  }

  const normalized = normalizeRequiredString("targetQuality", value);
  const [metric, rawValue] = normalized.split(":");

  if (rawValue === undefined || normalized.indexOf(":") !== normalized.lastIndexOf(":")) {
    throw new TypeError(
      "targetQuality must be <metric>:<value>, for example ssim:0.98",
    );
  }

  const metricName = metric.toLowerCase();
  if (metricName !== "ssim" && metricName !== "psnr") {
    throw new TypeError(`unsupported target quality metric \`${metric}\``);
  }
  if (rawValue.length === 0) {
    throw new TypeError("target quality value must be a number, got ``");
  }
  if (rawValue.trim() !== rawValue) {
    throw new TypeError(`target quality value must be a number, got \`${rawValue}\``);
  }

  const parsed = Number(rawValue);
  if (Number.isNaN(parsed)) {
    throw new TypeError(`target quality value must be a number, got \`${rawValue}\``);
  }
  if (!Number.isFinite(parsed)) {
    throw new TypeError("targetQuality must be finite");
  }

  if (metricName === "ssim" && (parsed <= 0.0 || parsed > 1.0)) {
    throw new TypeError("ssim targetQuality must be greater than 0.0 and at most 1.0");
  }
  if (metricName === "psnr" && parsed <= 0.0) {
    throw new TypeError("psnr targetQuality must be greater than 0");
  }

  return normalized;
}

function normalizeRemoteUrl(name, value) {
  const normalized = normalizeRequiredString(name, value);
  let url;

  try {
    url = new URL(normalized);
  } catch (error) {
    throw new TypeError(`${name} is invalid: ${error.message}`);
  }

  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new TypeError(`${name} must use the http or https scheme`);
  }
  if (url.username !== "" || url.password !== "") {
    throw new TypeError(`${name} must not embed user information`);
  }
  if (url.hostname === "") {
    throw new TypeError(`${name} must include a host`);
  }

  return normalized;
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
