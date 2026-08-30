import { signPublicUrl, type SignPublicUrlOptions } from "../../index.js";

const baseOptions: SignPublicUrlOptions = {
  baseUrl: "https://images.example.com",
  source: {
    kind: "path",
    path: "hero.jpg",
  },
  keyId: "public-demo",
  secret: new Uint8Array([1, 2, 3, 4]),
  expires: 1900000000,
};

const signedUrl: string = signPublicUrl({
  ...baseOptions,
  transforms: {
    width: 1200,
    height: 800,
    fit: "cover",
    format: "webp",
    optimize: "lossy",
    targetQuality: "ssim:0.98",
    rotate: 90,
  },
});

void signedUrl;

// Any whole angle is allowed, including negatives.
signPublicUrl({ ...baseOptions, transforms: { rotate: 45 } });
signPublicUrl({ ...baseOptions, transforms: { rotate: -90 } });

// @ts-expect-error rotate is a number of degrees, not a string
signPublicUrl({ ...baseOptions, transforms: { rotate: "90" } });

// @ts-expect-error format is constrained to documented output types
signPublicUrl({ ...baseOptions, transforms: { format: "gif" } });

signPublicUrl({
  ...baseOptions,
  transforms: {
    format: "webp",
    optimize: "lossy",
    // @ts-expect-error targetQuality uses the documented metric prefixes
    targetQuality: "butteraugli:1",
  },
});
