import { TrussImage } from "@/components/TrussImage";

export default function Home() {
  return (
    <main style={{ fontFamily: "system-ui, sans-serif", padding: "2rem" }}>
      <h1>truss + Next.js</h1>
      <p>
        Server-side image transformation using{" "}
        <a href="https://github.com/nao1215/truss">truss</a> with signed URLs.
      </p>

      <section>
        <h2>WebP, 400px wide</h2>
        <TrussImage
          src="sample.jpg"
          alt="Sample image converted to WebP at 400px width"
          width={400}
          format="webp"
          quality={80}
        />
      </section>

      <section>
        <h2>AVIF, 200x200 cover crop</h2>
        <TrussImage
          src="sample.jpg"
          alt="Sample image cropped to 200x200 square in AVIF format"
          width={200}
          height={200}
          fit="cover"
          format="avif"
          quality={60}
        />
      </section>

      <section>
        <h2>PNG, 300px wide</h2>
        <TrussImage
          src="sample.jpg"
          alt="Sample image converted to PNG at 300px width"
          width={300}
          format="png"
        />
      </section>
    </main>
  );
}
