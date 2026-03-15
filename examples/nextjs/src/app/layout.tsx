import type { Metadata } from "next";

export const metadata: Metadata = {
  title: "truss + Next.js Example",
  description: "Server-side image transformation with truss and Next.js",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
