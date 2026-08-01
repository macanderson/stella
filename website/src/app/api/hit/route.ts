import { bump, readCounters } from "@/lib/counters";

/**
 * The copy side of the install funnel. POST (the landing page's
 * `sendBeacon` on every successful copy) bumps `install:copies`; GET returns
 * both counters as JSON so the numbers are one curl away:
 *
 *     curl -s https://stella.oxagen.sh/api/hit
 *     {"configured":true,"hits":123,"copies":456}
 */

export async function POST(): Promise<Response> {
  await bump("copies");
  return new Response(null, { status: 204 });
}

export async function GET(): Promise<Response> {
  return Response.json(await readCounters(), {
    headers: { "cache-control": "no-store" },
  });
}
