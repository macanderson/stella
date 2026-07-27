import { ImageResponse } from "next/og";
import {
  MARK_PATH,
  MARK_VIEW_BOX,
  WORDMARK_PATH,
  WORDMARK_VIEW_BOX,
} from "@/components/brand";

/**
 * The social card, generated at build time (next/og) so it stays in sync with
 * the brand and carries no static binary.
 *
 * It is the brand lockup on the deepest ground: an electric mark beside a
 * paper wordmark on jet black. That split is the palette's rule 4 — the wordmark stays
 * ink/paper, the mark carries the hue — and it is what makes the card read as
 * Stella at thumbnail size, where the tagline is illegible anyway.
 *
 * Both glyphs are drawn from their outlines rather than set as text, so the
 * card shows the real logo instead of whatever face the renderer falls back to.
 *
 * Colours are literals rather than CSS vars because Satori resolves no
 * cascade; they are the tokens from docs/brand/tokens.json, named inline.
 * Measured on `night` #000000: wordmark 19.40:1, mark 8.19:1, tagline 8.50:1,
 * footer 4.71:1. Keep the markup inside Satori's supported subset — plain
 * <path> fills only, no gradients, masks, or filters.
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
          background: "#000000", // night
          color: "#f3f6fa", // text-primary
          padding: "80px",
        }}
      >
        <div style={{ display: "flex", alignItems: "center" }}>
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: "14px",
              border: "1px solid #26262e", // hairline
              borderRadius: "9999px",
              padding: "12px 26px",
              fontSize: "28px",
              color: "#98a6ba", // text-secondary
            }}
          >
            <span style={{ color: "#00aaff", fontWeight: 700 }}>{">_"}</span>
            <span>a terminal coding agent</span>
          </div>
        </div>

        <div style={{ display: "flex", flexDirection: "column" }}>
          {/* The lockup: electric mark, paper wordmark. */}
          <div style={{ display: "flex", alignItems: "center", gap: "36px" }}>
            <svg
              viewBox={MARK_VIEW_BOX}
              width={132}
              height={132}
              fill="#00aaff" // electric — the mark is the one place the hue lives
            >
              <path d={MARK_PATH} />
            </svg>
            <svg
              viewBox={WORDMARK_VIEW_BOX}
              width={440}
              height={130}
              fill="#f3f6fa" // text-primary — the wordmark never takes the accent
            >
              <path d={WORDMARK_PATH} />
            </svg>
          </div>
          <div
            style={{
              display: "flex",
              marginTop: "30px",
              fontSize: "42px",
              color: "#98a6ba", // text-secondary
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
            color: "#6c7b90", // text-tertiary — 4.85:1 on night
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
