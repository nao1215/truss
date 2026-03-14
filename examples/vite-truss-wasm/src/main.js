import "./style.css";
import {
  getCapabilitiesJson,
  inspectImageJson,
  transformImage,
} from "@nao1215/truss-wasm";

const SAMPLE_PNG_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABAQMAAAAl21bKAAAAIGNIUk0AAHomAACAhAAA+gAAAIDoAAB1MAAA6mAAADqYAAAXcJy6UTwAAAAGUExURf8AAP///0EdNBEAAAABYktHRAH/Ai3eAAAAB3RJTUUH6gMIDQMLTTjZYwAAAApJREFUCNdjYAAAAAIAAeIhvDMAAAAldEVYdGRhdGU6Y3JlYXRlADIwMjYtMDMtMDhUMTM6MDM6MTErMDA6MDAPBxgFAAAAJXRFWHRkYXRlOm1vZGlmeQAyMDI2LTAzLTA4VDEzOjAzOjExKzAwOjAwflqguQAAACh0RVh0ZGF0ZTp0aW1lc3RhbXAAMjAyNi0wMy0wOFQxMzowMzoxMSswMDowMClPgWYAAAAASUVORK5CYII=";

const app = document.querySelector("#app");
const capabilities = JSON.parse(getCapabilitiesJson());

app.innerHTML = `
  <main class="shell">
    <p class="eyebrow">Vite Consumer Example</p>
    <h1>Import, transform, ship.</h1>
    <p class="lede">
      This sample uses <code>@nao1215/truss-wasm</code> directly from a Vite app.
      It runs one transform on load, then lets you repeat the same pipeline with your own local image.
    </p>

    <div class="actions">
      <label class="picker">
        Choose Image
        <input id="file-input" type="file" accept="image/*" />
      </label>
      <span class="hint">The default run transforms an inline 1px PNG so the example works immediately.</span>
    </div>

    <div class="grid">
      <section class="panel">
        <h2>Output Preview</h2>
        <p id="status" class="status">Running transform...</p>
        <div class="preview">
          <img id="preview" alt="Transformed output preview" />
        </div>
      </section>

      <section class="panel">
        <h2>Transform Details</h2>
        <pre id="details"></pre>
      </section>

      <section class="panel">
        <h2>Capabilities</h2>
        <pre>${escapeJson(capabilities)}</pre>
      </section>

      <section class="panel">
        <h2>Install</h2>
        <pre>npm install @nao1215/truss-wasm</pre>
      </section>
    </div>
  </main>
`;

const fileInput = document.querySelector("#file-input");
const preview = document.querySelector("#preview");
const status = document.querySelector("#status");
const details = document.querySelector("#details");

let currentUrl;

fileInput.addEventListener("change", async (event) => {
  const file = event.target.files?.[0];
  if (!file) {
    return;
  }

  try {
    const inputBytes = new Uint8Array(await file.arrayBuffer());
    await runTransform(inputBytes, file.name);
  } catch (error) {
    setError(error);
  }
});

try {
  await runTransform(decodeBase64(SAMPLE_PNG_BASE64), "embedded 1px PNG");
} catch (error) {
  setError(error);
}

async function runTransform(inputBytes, label) {
  status.textContent = `Transforming ${label}...`;

  const inspected = JSON.parse(inspectImageJson(inputBytes, undefined));
  const result = transformImage(
    inputBytes,
    undefined,
    JSON.stringify({
      format: "jpeg",
      width: 240,
      height: 240,
      fit: "contain",
      background: "F4E8C8",
      quality: 82,
      autoOrient: true,
    }),
  );

  const response = JSON.parse(result.responseJson);
  const blob = new Blob([result.bytes], {
    type: response.artifact.mimeType,
  });

  if (currentUrl) {
    URL.revokeObjectURL(currentUrl);
  }

  currentUrl = URL.createObjectURL(blob);
  preview.src = currentUrl;
  details.textContent = escapeJson({
    source: label,
    inspected,
    response,
    outputBytes: result.bytes.length,
  });
  status.textContent = `Done: ${response.artifact.mimeType} ${response.artifact.width}x${response.artifact.height}`;
}

function decodeBase64(base64) {
  const decoded = atob(base64);
  return Uint8Array.from(decoded, (char) => char.charCodeAt(0));
}

function escapeJson(value) {
  return JSON.stringify(value, null, 2);
}

function setError(error) {
  const message = error instanceof Error ? error.message : String(error);
  status.textContent = `Transform failed: ${message}`;
  details.textContent = escapeJson({ error: message });
}
