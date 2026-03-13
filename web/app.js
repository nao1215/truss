import init, {
  getCapabilitiesJson,
  inspectImageJson,
  transformImage,
  transformImageWithWatermark,
} from "./pkg/truss.js";

const elements = {
  dropzone: document.querySelector("#dropzone"),
  fileInput: document.querySelector("#source-file"),
  inputPreview: document.querySelector("#input-preview"),
  inputPlaceholder: document.querySelector("#input-placeholder"),
  inputMeta: document.querySelector("#input-meta"),
  outputPreview: document.querySelector("#output-preview"),
  outputPlaceholder: document.querySelector("#output-placeholder"),
  outputMeta: document.querySelector("#output-meta"),
  capabilityStrip: document.querySelector("#capability-strip"),
  format: document.querySelector("#format"),
  width: document.querySelector("#width"),
  height: document.querySelector("#height"),
  fit: document.querySelector("#fit"),
  position: document.querySelector("#position"),
  rotate: document.querySelector("#rotate"),
  qualityField: document.querySelector("#quality-field"),
  qualityRange: document.querySelector("#quality-range"),
  qualityNumber: document.querySelector("#quality-number"),
  qualityNote: document.querySelector("#quality-note"),
  lockAspect: document.querySelector("#lock-aspect"),
  background: document.querySelector("#background"),
  metadataMode: document.querySelector("#metadata-mode"),
  blurRange: document.querySelector("#blur-range"),
  blurNumber: document.querySelector("#blur-number"),
  sharpenRange: document.querySelector("#sharpen-range"),
  sharpenNumber: document.querySelector("#sharpen-number"),
  watermarkFile: document.querySelector("#watermark-file"),
  watermarkDropzone: document.querySelector("#watermark-dropzone"),
  watermarkPreviewRow: document.querySelector("#watermark-preview-row"),
  watermarkPreview: document.querySelector("#watermark-preview"),
  watermarkClear: document.querySelector("#watermark-clear"),
  watermarkPosition: document.querySelector("#watermark-position"),
  watermarkOpacityRange: document.querySelector("#watermark-opacity-range"),
  watermarkOpacityNumber: document.querySelector("#watermark-opacity-number"),
  watermarkMargin: document.querySelector("#watermark-margin"),
  crop: document.querySelector("#crop"),
  autoOrient: document.querySelector("#auto-orient"),
  transformButton: document.querySelector("#transform-button"),
  downloadLink: document.querySelector("#download-link"),
  downloadNote: document.querySelector("#download-note"),
  statusLine: document.querySelector("#status-line"),
  warningList: document.querySelector("#warning-list"),
  errorBox: document.querySelector("#error-box"),
};

const state = {
  capabilities: null,
  inputFile: null,
  inputBytes: null,
  declaredMediaType: null,
  inputArtifact: null,
  inputObjectUrl: null,
  outputObjectUrl: null,
  watermarkBytes: null,
  watermarkObjectUrl: null,
  aspectRatio: null,
  updatingAspect: false,
};

boot().catch((error) => {
  showError(parseWasmError(error).message);
  setStatus("WASM runtime failed to load.");
});

async function boot() {
  await init();
  state.capabilities = JSON.parse(getCapabilitiesJson());

  wireEvents();
  renderCapabilities();
  refreshFormatState();
  refreshQualityState();
  refreshMetadataState();
  setStatus("WASM runtime is ready.");
}

function wireEvents() {
  elements.fileInput.addEventListener("change", async (event) => {
    const [file] = event.target.files ?? [];
    if (file) {
      await loadFile(file);
    }
  });

  ["dragenter", "dragover"].forEach((type) => {
    elements.dropzone.addEventListener(type, (event) => {
      event.preventDefault();
      elements.dropzone.classList.add("dragover");
    });
  });

  ["dragleave", "drop"].forEach((type) => {
    elements.dropzone.addEventListener(type, (event) => {
      event.preventDefault();
      elements.dropzone.classList.remove("dragover");
    });
  });

  elements.dropzone.addEventListener("drop", async (event) => {
    const [file] = event.dataTransfer?.files ?? [];
    if (file) {
      elements.fileInput.files = event.dataTransfer.files;
      await loadFile(file);
    }
  });

  elements.transformButton.addEventListener("click", runTransform);
  elements.format.addEventListener("change", () => {
    refreshQualityState();
    refreshMetadataState();
  });

  elements.width.addEventListener("input", () => {
    if (elements.lockAspect.checked && state.aspectRatio && !state.updatingAspect) {
      state.updatingAspect = true;
      const w = parseInteger(elements.width.value);
      if (w !== null) {
        elements.height.value = Math.max(1, Math.round(w / state.aspectRatio));
      }
      state.updatingAspect = false;
    }
    refreshFormatState();
  });

  elements.height.addEventListener("input", () => {
    if (elements.lockAspect.checked && state.aspectRatio && !state.updatingAspect) {
      state.updatingAspect = true;
      const h = parseInteger(elements.height.value);
      if (h !== null) {
        elements.width.value = Math.max(1, Math.round(h * state.aspectRatio));
      }
      state.updatingAspect = false;
    }
    refreshFormatState();
  });

  elements.qualityRange.addEventListener("input", () => {
    elements.qualityNumber.value = elements.qualityRange.value;
  });
  elements.qualityNumber.addEventListener("input", () => {
    const bounded = clampQuality(elements.qualityNumber.value);
    elements.qualityNumber.value = bounded;
    elements.qualityRange.value = bounded;
  });

  elements.blurRange.addEventListener("input", () => {
    elements.blurNumber.value = elements.blurRange.value;
  });
  elements.blurNumber.addEventListener("input", () => {
    const v = parseFloat(elements.blurNumber.value);
    if (Number.isFinite(v)) {
      const clamped = Math.min(100, Math.max(0, v));
      elements.blurNumber.value = clamped;
      elements.blurRange.value = clamped;
    }
  });

  elements.sharpenRange.addEventListener("input", () => {
    elements.sharpenNumber.value = elements.sharpenRange.value;
  });
  elements.sharpenNumber.addEventListener("input", () => {
    const v = parseFloat(elements.sharpenNumber.value);
    if (Number.isFinite(v)) {
      const clamped = Math.min(100, Math.max(0, v));
      elements.sharpenNumber.value = clamped;
      elements.sharpenRange.value = clamped;
    }
  });

  elements.watermarkFile.addEventListener("change", async (event) => {
    const [file] = event.target.files ?? [];
    if (file) await loadWatermark(file);
  });

  ["dragenter", "dragover"].forEach((type) => {
    elements.watermarkDropzone.addEventListener(type, (event) => {
      event.preventDefault();
      elements.watermarkDropzone.classList.add("dragover");
    });
  });

  ["dragleave", "drop"].forEach((type) => {
    elements.watermarkDropzone.addEventListener(type, (event) => {
      event.preventDefault();
      elements.watermarkDropzone.classList.remove("dragover");
    });
  });

  elements.watermarkDropzone.addEventListener("drop", async (event) => {
    const [file] = event.dataTransfer?.files ?? [];
    if (file) await loadWatermark(file);
  });
  elements.watermarkClear.addEventListener("click", clearWatermark);
  elements.watermarkOpacityRange.addEventListener("input", () => {
    elements.watermarkOpacityNumber.value = elements.watermarkOpacityRange.value;
  });
  elements.watermarkOpacityNumber.addEventListener("input", () => {
    const v = parseInt(elements.watermarkOpacityNumber.value, 10);
    if (Number.isFinite(v)) {
      const clamped = Math.min(100, Math.max(1, v));
      elements.watermarkOpacityNumber.value = clamped;
      elements.watermarkOpacityRange.value = clamped;
    }
  });

  [elements.inputPreview, elements.outputPreview].forEach((img) => {
    img.addEventListener("error", () => {
      if (!img.getAttribute("src")) return;
      img.hidden = true;
      const placeholder = img === elements.inputPreview
        ? elements.inputPlaceholder
        : elements.outputPlaceholder;
      placeholder.hidden = false;
      placeholder.textContent = img === elements.outputPreview
        ? "This browser cannot preview the transformed format, but the download is ready."
        : "Could not preview this image format.";
    });
  });
}

async function loadFile(file) {
  clearMessages();
  resetOutput();

  state.inputFile = file;
  state.inputBytes = new Uint8Array(await file.arrayBuffer());
  state.declaredMediaType = inferDeclaredMediaType(file);

  releaseUrl("inputObjectUrl");
  state.inputObjectUrl = URL.createObjectURL(file);
  showPreview(elements.inputPreview, elements.inputPlaceholder, state.inputObjectUrl);

  const response = JSON.parse(
    inspectImageJson(state.inputBytes, state.declaredMediaType ?? undefined),
  );
  state.inputArtifact = response.artifact;

  renderArtifactMeta(elements.inputMeta, response.artifact);

  if (response.artifact.width && response.artifact.height) {
    elements.width.value = response.artifact.width;
    elements.height.value = response.artifact.height;
    state.aspectRatio = response.artifact.width / response.artifact.height;
  } else {
    state.aspectRatio = null;
  }

  refreshFormatState();
  refreshMetadataState();

  if (response.artifact.mediaType === "svg" && !state.capabilities.svg) {
    setStatus("SVG input was loaded, but this browser build does not include SVG processing.");
    return;
  }

  if (response.artifact.mediaType === "avif" && !state.capabilities.avif) {
    setStatus("AVIF input was loaded, but this build cannot decode or encode AVIF.");
    return;
  }

  setStatus(`${file.name} loaded. Adjust options and run the transform.`);
}

async function runTransform() {
  clearMessages();

  if (!state.inputBytes || !state.inputArtifact) {
    showError("Choose an image before running the transform.");
    return;
  }

  const outputFormat = elements.format.value;
  if (state.inputArtifact.mediaType === "svg" && !state.capabilities.svg) {
    showError("This browser build does not include SVG processing.");
    return;
  }
  if (outputFormat === "svg" && state.inputArtifact.mediaType !== "svg") {
    showError("SVG output is only available when the input is already SVG.");
    return;
  }

  const options = collectOptions();

  setBusy(true);
  setStatus("Transforming in the browser.");
  await nextFrame();

  try {
    const hasWatermark = state.watermarkBytes !== null;
    const result = hasWatermark
      ? transformImageWithWatermark(
          state.inputBytes,
          state.declaredMediaType ?? undefined,
          JSON.stringify(options),
          state.watermarkBytes,
          JSON.stringify({
            position: elements.watermarkPosition.value,
            opacity: Number(elements.watermarkOpacityNumber.value),
            margin: Number(elements.watermarkMargin.value),
          }),
        )
      : transformImage(
          state.inputBytes,
          state.declaredMediaType ?? undefined,
          JSON.stringify(options),
        );
    const response = JSON.parse(result.responseJson);
    const outputBytes = result.bytes;
    const outputBlob = new Blob([outputBytes], { type: response.artifact.mimeType });

    releaseUrl("outputObjectUrl");
    state.outputObjectUrl = URL.createObjectURL(outputBlob);

    showPreview(elements.outputPreview, elements.outputPlaceholder, state.outputObjectUrl);
    renderArtifactMeta(elements.outputMeta, response.artifact);
    renderWarnings(response.warnings);
    updateDownloadLink(response, state.outputObjectUrl, hasWatermark);
    const suffix = hasWatermark ? " (with watermark)" : "";
    setStatus(
      response.warnings.length
        ? `Transform finished with warnings${suffix}.`
        : `Transform finished${suffix}.`,
    );
  } catch (error) {
    const payload = parseWasmError(error);
    showError(payload.message);
    setStatus("Transform failed.");
  } finally {
    setBusy(false);
  }
}

function collectOptions() {
  const width = parseInteger(elements.width.value);
  const height = parseInteger(elements.height.value);
  const outputFormat = elements.format.value;
  const boundedResize = width !== null && height !== null;
  const metadataMode = elements.metadataMode.value;

  return {
    format: outputFormat,
    width,
    height,
    fit: boundedResize ? emptyToNull(elements.fit.value) : null,
    position: boundedResize ? emptyToNull(elements.position.value) : null,
    rotate: Number(elements.rotate.value),
    quality: qualityEnabled() ? Number(elements.qualityNumber.value) : null,
    background: normalizeHex(elements.background.value),
    autoOrient: elements.autoOrient.checked,
    keepMetadata: metadataMode === "keep",
    preserveExif: metadataMode === "exif",
    crop: emptyToNull(elements.crop.value.trim()),
    blur: (() => { const v = Math.max(0, parseFloat(elements.blurNumber.value) || 0); return v >= 0.1 ? v : null; })(),
    sharpen: (() => { const v = Math.max(0, parseFloat(elements.sharpenNumber.value) || 0); return v >= 0.1 ? v : null; })(),
  };
}

function refreshFormatState() {
  const svgOption = elements.format.querySelector('option[value="svg"]');
  const svgAllowed = state.capabilities?.svg && state.inputArtifact?.mediaType === "svg";

  if (svgOption) {
    svgOption.disabled = !svgAllowed;
    svgOption.textContent = svgAllowed ? "SVG" : "SVG (input must be SVG)";
  }

  if (elements.format.value === "svg" && !svgAllowed) {
    elements.format.value = state.inputArtifact?.mediaType === "png" ? "png" : "jpeg";
  }

  const avifOption = elements.format.querySelector('option[value="avif"]');
  if (avifOption) {
    avifOption.disabled = !state.capabilities?.avif;
    avifOption.textContent = state.capabilities?.avif ? "AVIF" : "AVIF (not in this build)";
  }

  if (elements.format.value === "avif" && !state.capabilities?.avif) {
    elements.format.value = "jpeg";
  }

  refreshQualityState();
}

function refreshQualityState() {
  const format = elements.format.value;
  const lossyWebpUnavailable = format === "webp" && !state.capabilities?.webpLossy;
  const enabled = qualityEnabled();

  elements.qualityField.classList.toggle("is-unavailable", lossyWebpUnavailable);
  elements.qualityRange.disabled = !enabled;
  elements.qualityNumber.disabled = !enabled;
  elements.qualityField.hidden = !enabled && !lossyWebpUnavailable;

  if (lossyWebpUnavailable) {
    elements.qualityField.hidden = false;
    elements.qualityNote.textContent =
      "This build keeps WebP output lossless. JPEG and AVIF still accept quality.";
  } else {
    elements.qualityNote.textContent =
      "Quality applies to JPEG, AVIF, and WebP when lossy WebP is enabled.";
  }
}

function refreshMetadataState() {
  const format = elements.format.value;
  const keepOption = elements.metadataMode.querySelector('option[value="keep"]');
  const exifOption = elements.metadataMode.querySelector('option[value="exif"]');
  const allowMetadataRetention = format !== "avif" && format !== "bmp" && format !== "svg";

  keepOption.disabled = !allowMetadataRetention;
  exifOption.disabled = !allowMetadataRetention;

  if (!allowMetadataRetention && elements.metadataMode.value !== "strip") {
    elements.metadataMode.value = "strip";
  }
}

function renderCapabilities() {
  const messages = [
    {
      title: "Runtime",
      copy: "WASM build running in your browser.",
    },
    {
      title: "SVG",
      copy: state.capabilities.svg ? "Enabled in this build." : "Excluded from this build.",
    },
    {
      title: "WebP quality",
      copy: state.capabilities.webpLossy
        ? "Lossy WebP is available."
        : "WebP stays lossless here.",
    },
    {
      title: "AVIF",
      copy: state.capabilities.avif
        ? "Decode and encode available."
        : "Not available in this build.",
    },
  ];

  elements.capabilityStrip.replaceChildren(
    ...messages.map((item) => {
      const pill = document.createElement("span");
      pill.className = "capability-pill";
      pill.innerHTML = `<strong>${item.title}</strong> ${item.copy}`;
      return pill;
    }),
  );
}

function renderArtifactMeta(container, artifact) {
  const values = [
    artifact.mediaType.toUpperCase(),
    artifact.width && artifact.height ? `${artifact.width} × ${artifact.height}` : "Unknown",
    artifact.hasAlpha === null ? "Unknown" : artifact.hasAlpha ? "Yes" : "No",
    String(artifact.frameCount),
  ];

  [...container.querySelectorAll("dd")].forEach((node, index) => {
    node.textContent = values[index];
  });
}

function renderWarnings(warnings) {
  elements.warningList.replaceChildren();

  if (!warnings.length) {
    elements.warningList.hidden = true;
    return;
  }

  warnings.forEach((warning) => {
    const item = document.createElement("li");
    item.textContent = warning;
    elements.warningList.append(item);
  });
  elements.warningList.hidden = false;
}

function updateDownloadLink(response, href, hasWatermark) {
  const extension = response.suggestedExtension;
  const stem = state.inputFile?.name?.replace(/\.[^.]+$/, "") || "truss-output";
  const suffix = hasWatermark ? "-watermarked" : "";
  const filename = `${stem}-truss${suffix}.${extension}`;

  elements.downloadLink.href = href;
  elements.downloadLink.download = filename;
  elements.downloadLink.classList.remove("disabled");
  elements.downloadNote.textContent = filename;
}

const WATERMARK_ACCEPTED_TYPES = new Set([
  "image/png", "image/jpeg", "image/webp", "image/bmp", "image/x-ms-bmp", "image/x-windows-bmp",
]);
const WATERMARK_MAX_BYTES = 10 * 1024 * 1024;

async function loadWatermark(file) {
  if (file.type && !WATERMARK_ACCEPTED_TYPES.has(file.type)) {
    showError("Watermark must be PNG, JPEG, WebP, or BMP. SVG is not supported.");
    return;
  }
  if (file.size > WATERMARK_MAX_BYTES) {
    showError(`Watermark file is too large (${(file.size / 1024 / 1024).toFixed(1)} MB). Maximum is 10 MB.`);
    return;
  }
  setStatus("Loading watermark\u2026");
  state.watermarkBytes = new Uint8Array(await file.arrayBuffer());
  releaseUrl("watermarkObjectUrl");
  state.watermarkObjectUrl = URL.createObjectURL(file);
  elements.watermarkPreview.src = state.watermarkObjectUrl;
  elements.watermarkPreviewRow.hidden = false;
  elements.errorBox.hidden = true;
  elements.errorBox.textContent = "";
  setStatus(`Watermark "${file.name}" loaded.`);
}

function clearWatermark() {
  state.watermarkBytes = null;
  releaseUrl("watermarkObjectUrl");
  elements.watermarkPreviewRow.hidden = true;
  elements.watermarkPreview.removeAttribute("src");
  elements.watermarkFile.value = "";
  setStatus("Watermark removed.");
}

function resetOutput() {
  releaseUrl("outputObjectUrl");
  hidePreview(elements.outputPreview, elements.outputPlaceholder, "Transform output will appear here.");
  renderArtifactMeta(elements.outputMeta, {
    mediaType: "—",
    width: null,
    height: null,
    hasAlpha: null,
    frameCount: "—",
  });
  elements.downloadLink.href = "#";
  elements.downloadLink.classList.add("disabled");
  elements.downloadNote.textContent = "Nothing has been transformed yet.";
}

function clearMessages() {
  elements.errorBox.hidden = true;
  elements.errorBox.textContent = "";
  renderWarnings([]);
}

function showError(message) {
  elements.errorBox.textContent = message;
  elements.errorBox.hidden = false;
}

function showPreview(image, placeholder, url) {
  image.hidden = false;
  image.src = url;
  placeholder.hidden = true;
}

function hidePreview(image, placeholder, copy) {
  image.hidden = true;
  image.removeAttribute("src");
  placeholder.hidden = false;
  placeholder.textContent = copy;
}

function setBusy(active) {
  document.body.classList.toggle("is-busy", active);
  elements.transformButton.disabled = active;
  elements.transformButton.textContent = active
    ? "Transforming…"
    : "Transform in browser";
}

function setStatus(message) {
  elements.statusLine.textContent = message;
}

function releaseUrl(key) {
  if (state[key]) {
    URL.revokeObjectURL(state[key]);
    state[key] = null;
  }
}

function inferDeclaredMediaType(file) {
  const byMime = {
    "image/jpeg": "jpeg",
    "image/png": "png",
    "image/webp": "webp",
    "image/avif": "avif",
    "image/bmp": "bmp",
    "image/x-ms-bmp": "bmp",
    "image/x-windows-bmp": "bmp",
    "image/tiff": "tiff",
    "image/svg+xml": "svg",
  };
  if (file.type && byMime[file.type]) {
    return byMime[file.type];
  }

  const extension = file.name.split(".").pop()?.toLowerCase();
  const byExtension = {
    jpg: "jpeg",
    jpeg: "jpeg",
    png: "png",
    webp: "webp",
    avif: "avif",
    bmp: "bmp",
    tiff: "tiff",
    tif: "tiff",
    svg: "svg",
  };
  return byExtension[extension] ?? null;
}

function parseInteger(value) {
  if (!value.trim()) {
    return null;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) ? parsed : null;
}

function emptyToNull(value) {
  return value ? value : null;
}

function normalizeHex(value) {
  const trimmed = value.trim().replace(/^#/, "").toUpperCase();
  return trimmed ? trimmed : null;
}

function clampQuality(value) {
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed)) {
    return "82";
  }
  return String(Math.min(100, Math.max(1, parsed)));
}

function qualityEnabled() {
  const format = elements.format.value;
  return (
    format === "jpeg" ||
    (format === "avif" && state.capabilities?.avif) ||
    (format === "webp" && state.capabilities?.webpLossy)
  );
}

function parseWasmError(error) {
  const message = typeof error === "string" ? error : String(error);

  try {
    return JSON.parse(message);
  } catch {
    return { kind: "runtimeError", message };
  }
}

function nextFrame() {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}
