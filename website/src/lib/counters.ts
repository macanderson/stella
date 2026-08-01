import { Redis } from "@upstash/redis";

/**
 * The install funnel's two counters — script downloads (`install:hits`,
 * bumped by /install.sh) and command copies (`install:copies`, bumped by the
 * landing page's beacon at /api/hit).
 *
 * Storage is Upstash Redis via the Vercel Marketplace (`vercel integration
 * add upstash`), which injects UPSTASH_REDIS_REST_URL/TOKEN. `INCR` is
 * atomic, so concurrent hits never lose counts — the failure mode a
 * read-modify-write over blob storage would have.
 *
 * Everything degrades to a no-op when the store is not provisioned, and
 * every call swallows its errors: counting is a passenger. It must never
 * delay, break, or block serving the install script — a lost count is
 * nothing, a failed install is a lost user.
 */

export type InstallCounter = "hits" | "copies";

function client(): Redis | null {
  // Checked per call rather than at module top level so `next build` (which
  // evaluates module scope) never requires the env to exist.
  if (
    !process.env.UPSTASH_REDIS_REST_URL ||
    !process.env.UPSTASH_REDIS_REST_TOKEN
  ) {
    return null;
  }
  return Redis.fromEnv();
}

/**
 * Add one to a counter. Fire-and-forget semantics; never throws. Each bump
 * (and each failure) emits one structured line so Vercel's function logs
 * carry the funnel too — searchable as `install-funnel` in the dashboard.
 */
export async function bump(counter: InstallCounter): Promise<void> {
  const redis = client();
  try {
    const total = await redis?.incr(`install:${counter}`);
    console.log(
      JSON.stringify({
        event: "install-funnel",
        counter,
        total: total ?? null,
        stored: redis !== null,
      }),
    );
  } catch (error) {
    // Counting is best-effort by design — log it, never surface it.
    console.error(
      JSON.stringify({
        event: "install-funnel-error",
        counter,
        message: error instanceof Error ? error.message : String(error),
      }),
    );
  }
}

/** Both counters. `configured: false` means the store is not provisioned. */
export async function readCounters(): Promise<{
  configured: boolean;
  hits: number;
  copies: number;
}> {
  try {
    const redis = client();
    if (!redis) return { configured: false, hits: 0, copies: 0 };
    const [hits, copies] = await redis.mget<(number | null)[]>(
      "install:hits",
      "install:copies",
    );
    return { configured: true, hits: hits ?? 0, copies: copies ?? 0 };
  } catch {
    return { configured: false, hits: 0, copies: 0 };
  }
}
