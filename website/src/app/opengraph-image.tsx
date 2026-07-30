import { ImageResponse } from "next/og";
import {
  CURSOR_RECT,
  MARK_PATH,
  WORDMARK_PATH,
  WORDMARK_VIEW_BOX,
} from "@/components/brand";

/**
 * The social card, generated at build time (next/og) so it stays in sync with
 * the brand and ships no static binary.
 *
 * It is the lockup on the deepest ground: the mark and wordmark in `text-
 * primary`, the cursor block in `gold`. That split is BRAND.md's Logo table —
 * the glyph and wordmark are ink, the cursor carries the identity gold — and it
 * is what makes the card read as Stella at thumbnail size, where the tagline is
 * illegible anyway.
 *
 * Both glyphs are drawn from their outlines rather than set as text, so the
 * card shows the real logo instead of whatever face the renderer falls back to.
 *
 * Colours are literals rather than CSS vars because Satori resolves no cascade.
 * They are the tokens from src/app/tokens.css, named inline — edit them
 * together. Measured on `void` #05070C: wordmark 18.4:1, cursor 12.1:1, tagline
 * 6.9:1, footer 4.9:1. Keep the markup inside Satori's supported subset — plain
 * <path>/<rect> fills only, no gradients, masks, or filters.
 */
export const alt =
  "Stella — a fast, BYOK, model-agnostic terminal coding agent that proves its work";
export const size = { width: 1200, height: 630 };
export const contentType = "image/png";

export default function OpengraphImage() {
  return new ImageResponse(
    (
      <div
        style={{
          width: "100%",
          height: "100%",
          display: "flex",
          flexDirection: "column",
          justifyContent: "space-between",
          background: "#05070c", // void
          color: "#f2f5fa", // text-primary
          padding: "80px",
        }}
      >
        <div style={{ display: "flex", alignItems: "center" }}>
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: "14px",
              fontSize: "28px",
              letterSpacing: "0.12em",
              textTransform: "uppercase",
              color: "#8e97a8", // text-secondary
            }}
          >
            <span style={{ color: "#f5c145" }}>{"▊"}</span>
            <span>a terminal coding agent</span>
          </div>
        </div>

        <div style={{ display: "flex", flexDirection: "column" }}>
          <div style={{ display: "flex", alignItems: "center", gap: "40px" }}>
            <svg viewBox="0 0 148 100" width={196} height={132}>
              <path d={MARK_PATH} fill="#f2f5fa" />
              <rect {...CURSOR_RECT} fill="#f5c145" />
            </svg>
            <svg
              viewBox={WORDMARK_VIEW_BOX}
              width={440}
              height={130}
              fill="#f2f5fa"
            >
              <path d={WORDMARK_PATH} />
            </svg>
          </div>
          <div
            style={{
              display: "flex",
              marginTop: "30px",
              fontSize: "42px",
              color: "#8e97a8", // text-secondary
              maxWidth: "940px",
              lineHeight: 1.25,
            }}
          >
            Ship code from your terminal — and let a verifier decide when the
            work is actually done.
          </div>
        </div>

        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            fontSize: "28px",
            color: "#737d92", // text-tertiary — 4.9:1 on void
          }}
        >
          <div style={{ display: "flex" }}>stella.oxagen.sh</div>
          <div style={{ display: "flex" }}>
            BYOK · model-agnostic · local-first
          </div>
        </div>
      </div>
    ),
    { ...size },
  );
}
