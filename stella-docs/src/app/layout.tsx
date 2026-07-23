import "./global.css";
import type { Metadata, Viewport } from "next";
import type { ReactNode } from "react";
import { RootProvider } from "fumadocs-ui/provider/next";

const SITE_URL = "https://stella.oxagen.sh";
const SITE_NAME = "Stella CLI";
const SITE_DESCRIPTION =
  "Documentation for the Stella CLI — a fast, BYOK, model-agnostic terminal coding agent.";
const OG_IMAGE = "/social/og-card.png";

export const metadata: Metadata = {
  metadataBase: new URL(SITE_URL),
  title: {
    default: "Stella CLI — Docs",
    template: "%s — Stella CLI",
  },
  description: SITE_DESCRIPTION,
  applicationName: SITE_NAME,
  appleWebApp: {
    capable: true,
    title: "Stella",
    statusBarStyle: "black-translucent",
  },
  openGraph: {
    title: "Stella CLI — Docs",
    description: SITE_DESCRIPTION,
    url: SITE_URL,
    siteName: SITE_NAME,
    type: "website",
    images: [
      {
        url: OG_IMAGE,
        width: 1200,
        height: 630,
        alt: "Stella — a fast, BYOK, model-agnostic terminal coding agent",
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    title: "Stella CLI — Docs",
    description: SITE_DESCRIPTION,
    images: [OG_IMAGE],
  },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  themeColor: [
    { media: "(prefers-color-scheme: dark)", color: "#000000" },
    { media: "(prefers-color-scheme: light)", color: "#ffffff" },
  ],
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" suppressHydrationWarning>
      <body className="flex min-h-screen flex-col">
        <RootProvider>{children}</RootProvider>
      </body>
    </html>
  );
}
