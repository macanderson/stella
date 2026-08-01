import { bump } from "@/lib/counters";

/**
 * Serves the install script from the site's own domain — `curl -fsSL
 * https://stella.oxagen.sh/install.sh | sh` — so every real install run is
 * countable. The script itself stays canonical in the repository; this route
 * proxies GitHub raw (cached upstream for 5 minutes via the data cache) and
 * bumps `install:hits` on every request.
 *
 * The response is `no-store`: a CDN-cached copy would serve installs the
 * function never sees, and the count exists to be believed. If the upstream
 * fetch fails, redirect to GitHub raw instead of failing — the one-liner
 * uses -L, so a degraded counter must never become a degraded install.
 */

const UPSTREAM =
  "https://raw.githubusercontent.com/macanderson/stella/main/install.sh";

export async function GET(): Promise<Response> {
  // Count first: a hit that ends in the fallback redirect is still a hit.
  await bump("hits");

  let script: string;
  try {
    const upstream = await fetch(UPSTREAM, { next: { revalidate: 300 } });
    if (!upstream.ok) throw new Error(`upstream ${upstream.status}`);
    script = await upstream.text();
  } catch (error) {
    // The redirect path still installs (the one-liner uses -L); the log line
    // is what tells us the proxy is degraded before anyone notices.
    console.error(
      JSON.stringify({
        event: "install-sh-fallback",
        message: error instanceof Error ? error.message : String(error),
      }),
    );
    return Response.redirect(UPSTREAM, 302);
  }

  return new Response(script, {
    headers: {
      "content-type": "text/x-shellscript; charset=utf-8",
      "cache-control": "no-store",
    },
  });
}
