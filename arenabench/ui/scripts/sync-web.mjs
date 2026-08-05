// Sync the static export into arenabench/web/ — the directory the Python
// server serves and the wheel ships. Node-only so the build works the same
// on any platform CI runs.
import { cpSync, existsSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "..", "out");
const web = join(here, "..", "..", "arenabench", "web");

if (!existsSync(join(out, "index.html"))) {
  console.error("sync-web: no export at ui/out — did `next build` run?");
  process.exit(1);
}

rmSync(web, { recursive: true, force: true });
cpSync(out, web, { recursive: true });
writeFileSync(
  join(web, "GENERATED.md"),
  "This directory is the built ArenaBench client. Do not edit it by hand —\n" +
    "the source lives in ui/ and `npm run build` regenerates everything here.\n",
);
console.log(`sync-web: exported client -> ${web}`);
