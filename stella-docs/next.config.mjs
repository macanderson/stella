import { createMDX } from "fumadocs-mdx/next";

const withMDX = createMDX();

/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  // The site is fully static (MDX + generateStaticParams); no image
  // optimization server is needed.
  images: {
    unoptimized: true,
  },
};

export default withMDX(nextConfig);
