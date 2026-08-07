// Interim access gate for arenabench.org.
//
// The Vercel plan does not offer team authentication on production
// domains, and the page exposes spend-triggering actions, so production
// cannot be public. Until the real login lands (#2100: Auth.js magic
// links via Resend, allowlisted addresses), entry requires a one-time
// visit to /unlock?key=<ACCESS_KEY>, which sets a cookie.
import { NextResponse } from "next/server";

export const config = {
  matcher: ["/((?!_next/|favicon\\.ico|unlock).*)"],
};

export function middleware(req) {
  const key = process.env.ACCESS_KEY;
  if (!key) return NextResponse.next();
  if (req.cookies.get("ab_key")?.value === key) return NextResponse.next();
  return new NextResponse(
    "ArenaBench is locked. Open your access link (/unlock?key=...) once to enter.\n",
    { status: 401, headers: { "content-type": "text/plain" } },
  );
}
