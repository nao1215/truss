import assert from "node:assert/strict";
import test from "node:test";

import { signPublicUrl } from "../index.js";

test("matches the fixed compatibility vector from the Rust signer", () => {
  const signedUrl = signPublicUrl({
    baseUrl: "https://images.example.com",
    source: {
      kind: "path",
      path: "image.png",
    },
    transforms: {
      width: 800,
      format: "webp",
    },
    keyId: "public-demo",
    secret: "secret-value",
    expires: 1900000000,
  });

  assert.equal(
    signedUrl,
    "https://images.example.com/images/by-path?expires=1900000000&format=webp&keyId=public-demo&path=image.png&signature=8c3234125e0e20efeaae1e2afaa88a81d387c82cef0080780fddd31c5689199e&width=800",
  );
});

test("signs remote URL requests with optional transform and watermark fields", () => {
  const signedUrl = signPublicUrl({
    baseUrl: "https://images.example.com",
    source: {
      kind: "url",
      url: "https://origin.example.com/photo.png",
      version: "v2",
    },
    transforms: {
      width: 640,
      height: 480,
      fit: "cover",
      format: "avif",
      optimize: "lossy",
      targetQuality: "ssim:0.98",
      rotate: 270,
      autoOrient: false,
      stripMetadata: false,
      preserveExif: true,
      blur: 1.5,
    },
    watermark: {
      url: "https://cdn.example.com/logo.png",
      position: "bottom-right",
      opacity: 50,
      margin: 12,
    },
    preset: "marketing",
    keyId: "public-demo",
    secret: "secret-value",
    expires: 1900000000,
  });

  const url = new URL(signedUrl);

  assert.equal(url.pathname, "/images/by-url");
  assert.equal(url.searchParams.get("url"), "https://origin.example.com/photo.png");
  assert.equal(url.searchParams.get("version"), "v2");
  assert.equal(url.searchParams.get("optimize"), "lossy");
  assert.equal(url.searchParams.get("targetQuality"), "ssim:0.98");
  assert.equal(url.searchParams.get("rotate"), "270");
  assert.equal(url.searchParams.get("autoOrient"), "false");
  assert.equal(url.searchParams.get("stripMetadata"), "false");
  assert.equal(url.searchParams.get("preserveExif"), "true");
  assert.equal(url.searchParams.get("watermarkUrl"), "https://cdn.example.com/logo.png");
  assert.equal(url.searchParams.get("watermarkPosition"), "bottom-right");
  assert.equal(url.searchParams.get("watermarkOpacity"), "50");
  assert.equal(url.searchParams.get("watermarkMargin"), "12");
  assert.match(url.searchParams.get("signature"), /^[0-9a-f]{64}$/);
});

test("matches the Rust canonicalization for HEAD requests with preset and watermark", () => {
  const signedUrl = signPublicUrl({
    baseUrl: "https://images.example.com",
    source: {
      kind: "url",
      url: "https://origin.example.com/banner.png",
      version: "v4",
    },
    transforms: {
      width: 1200,
      height: 628,
      fit: "cover",
      position: "top",
      format: "webp",
      optimize: "lossy",
      targetQuality: "psnr:41",
      background: "ffffff",
      rotate: 180,
      stripMetadata: false,
      crop: "0,0,1200,628",
      sharpen: 1.25,
    },
    watermark: {
      url: "https://cdn.example.com/logo.png",
      position: "bottom-right",
      opacity: 70,
      margin: 24,
    },
    preset: "social-card",
    keyId: "public-demo",
    secret: "secret-value",
    expires: 1900000000,
    method: "HEAD",
  });

  assert.equal(
    signedUrl,
    "https://images.example.com/images/by-url?background=FFFFFF&crop=0%2C0%2C1200%2C628&expires=1900000000&fit=cover&format=webp&height=628&keyId=public-demo&optimize=lossy&position=top&preset=social-card&rotate=180&sharpen=1.25&signature=3ffe4b1775f495aa660f57336a7fe4f79ea65bca78bc7f151d003707965729d5&stripMetadata=false&targetQuality=psnr%3A41&url=https%3A%2F%2Forigin.example.com%2Fbanner.png&version=v4&watermarkMargin=24&watermarkOpacity=70&watermarkPosition=bottom-right&watermarkUrl=https%3A%2F%2Fcdn.example.com%2Flogo.png&width=1200",
  );
});

test("rejects invalid base URLs and expires values", () => {
  assert.throws(
    () =>
      signPublicUrl({
        baseUrl: "ftp://images.example.com",
        source: { kind: "path", path: "image.png" },
        keyId: "public-demo",
        secret: "secret-value",
        expires: 1900000000,
      }),
    /http or https/,
  );

  assert.throws(
    () =>
      signPublicUrl({
        baseUrl: "https://images.example.com",
        source: { kind: "path", path: "image.png" },
        keyId: "public-demo",
        secret: "secret-value",
        expires: -1,
      }),
    /expires/,
  );

  assert.throws(
    () =>
      signPublicUrl({
        baseUrl: "https://images.example.com",
        source: { kind: "path", path: "image.png" },
        keyId: "public-demo",
        secret: "secret-value",
        expires: 0,
      }),
    />= 1/,
  );
});

test("rejects invalid source and watermark URLs", () => {
  assert.throws(
    () =>
      signPublicUrl({
        baseUrl: "https://images.example.com",
        source: {
          kind: "url",
          url: "ftp://origin.example.com/image.png",
        },
        keyId: "public-demo",
        secret: "secret-value",
        expires: 1900000000,
      }),
    /http or https/,
  );

  assert.throws(
    () =>
      signPublicUrl({
        baseUrl: "https://images.example.com",
        source: { kind: "path", path: "image.png" },
        watermark: {
          url: "https://user:pass@cdn.example.com/logo.png",
        },
        keyId: "public-demo",
        secret: "secret-value",
        expires: 1900000000,
      }),
    /must not embed user information/,
  );
});

test("rejects transform combinations that truss would reject", () => {
  const baseOptions = {
    baseUrl: "https://images.example.com",
    source: { kind: "path", path: "image.png" },
    keyId: "public-demo",
    secret: "secret-value",
    expires: 1900000000,
  };
  const cases = [
    {
      options: { transforms: { quality: 0 } },
      pattern: /quality must be between 1 and 100/,
    },
    {
      options: { transforms: { rotate: 45 } },
      pattern: /rotate must be 0, 90, 180, or 270/,
    },
    {
      options: { transforms: { width: 320, fit: "cover" } },
      pattern: /fit requires both width and height/,
    },
    {
      options: { transforms: { height: 320, position: "top" } },
      pattern: /position requires both width and height/,
    },
    {
      options: { transforms: { preserveExif: true } },
      pattern: /preserveExif requires stripMetadata to be false/,
    },
    {
      options: {
        transforms: { format: "jpeg", quality: 80, optimize: "lossless" },
      },
      pattern: /quality cannot be combined with optimize=lossless/,
    },
    {
      options: {
        transforms: { format: "jpeg", targetQuality: "ssim:0.98" },
      },
      pattern: /targetQuality requires optimize=auto or optimize=lossy/,
    },
    {
      options: {
        transforms: {
          format: "png",
          optimize: "auto",
          targetQuality: "ssim:0.98",
        },
      },
      pattern: /targetQuality requires jpeg, webp, or avif output/,
    },
    {
      options: {
        transforms: {
          format: "jpeg",
          optimize: "lossy",
          targetQuality: "SSIM:0.98",
        },
      },
      pattern: /unsupported target quality metric/,
    },
    {
      options: {
        transforms: {
          format: "jpeg",
          optimize: "lossy",
          targetQuality: "ssim:1e-1",
        },
      },
      pattern: /target quality value must be a number/,
    },
    {
      options: {
        transforms: {
          format: "jpeg",
          optimize: "lossy",
          targetQuality: "psnr:01",
        },
      },
      pattern: /target quality value must be a number/,
    },
    {
      options: { transforms: { format: "svg", optimize: "auto" } },
      pattern: /optimization is not supported for svg output/,
    },
    {
      options: {
        transforms: { crop: "0,0,0,10" },
      },
      pattern: /crop width and height must be greater than zero/,
    },
    {
      options: {
        transforms: { background: "#ffffff" },
      },
      pattern: /unsupported color/,
    },
    {
      options: {
        transforms: { blur: 0.0 },
      },
      pattern: /blur sigma must be between 0.1 and 100.0/,
    },
    {
      options: {
        transforms: { sharpen: 100.1 },
      },
      pattern: /sharpen sigma must be between 0.1 and 100.0/,
    },
    {
      options: {
        watermark: { url: "https://cdn.example.com/logo.png", opacity: 0 },
      },
      pattern: /watermarkOpacity must be between 1 and 100/,
    },
  ];

  for (const { options, pattern } of cases) {
    assert.throws(() => signPublicUrl({ ...baseOptions, ...options }), pattern);
  }
});

test("accepts the inclusive upper blur boundary", () => {
  assert.doesNotThrow(() =>
    signPublicUrl({
      baseUrl: "https://images.example.com",
      source: { kind: "path", path: "image.png" },
      transforms: { blur: 100.0 },
      keyId: "public-demo",
      secret: "secret-value",
      expires: 1900000000,
    }),
  );
});
