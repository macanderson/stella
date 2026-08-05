import type { NextConfig } from "next";

const isDev = process.env.NODE_ENV === "development";

const nextConfig: NextConfig = {
  // Production builds are a pure static export: the Python server in
  // arenabench/server.py serves the files and owns every /api route. In dev
  // we skip the export so the rewrite below can proxy /api to a running
  // `arenabench serve` — same origin, so SSE needs no CORS story.
  ...(isDev ? {} : { output: "export" as const }),

  // A stable build id keeps rebuilds from churning the committed export for
  // no reason. Chunk hashes still change when content changes.
  generateBuildId: () => "arenabench",

  // Only attached in dev: the export build has no server to rewrite with,
  // and defining it there just makes `next build` warn.
  ...(isDev
    ? {
        async rewrites() {
          const api = process.env.ARENA_API ?? "http://127.0.0.1:8900";
          return [{ source: "/api/:path*", destination: `${api}/api/:path*` }];
        },
      }
    : {}),
};

export default nextConfig;
