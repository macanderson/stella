// One-time unlock: exchanges ?key=<ACCESS_KEY> for the gate cookie.
// See middleware.js for why this interim gate exists; #2100 replaces it.
import { NextResponse } from "next/server";

export async function GET(req) {
  const url = new URL(req.url);
  const key = url.searchParams.get("key") ?? "";
  if (!process.env.ACCESS_KEY || key !== process.env.ACCESS_KEY) {
    return new NextResponse("invalid key\n", { status: 401 });
  }
  const res = NextResponse.redirect(new URL("/", req.url));
  res.cookies.set("ab_key", key, {
    httpOnly: true,
    secure: true,
    sameSite: "lax",
    maxAge: 60 * 60 * 24 * 30,
    path: "/",
  });
  return res;
}
