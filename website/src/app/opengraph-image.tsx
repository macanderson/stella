import { ImageResponse } from "next/og";
import {
  GOLD,
  STAR_PATH,
  TRAIL_RECTS,
  WORDMARK_LETTERS_PATH,
  WORDMARK_SPARKLE_PATH,
  WORDMARK_VIEW_BOX,
} from "@/components/brand";

/**
 * The social card, generated at build time (next/og) so it stays in sync with
 * the brand and ships no static binary.
 *
 * It is the lockup on Ink: the gold comet flying left→right into the outlined
 * wordmark, Paper letters, gold sparkle — exactly docs/brand's OG art,
 * redrawn from the same geometry. Both marks are paths rather than text, so
 * the card shows the real logo instead of whatever face the renderer falls
 * back to.
 *
 * Colours are literals or imported constants rather than CSS vars because
 * Satori resolves no cascade: Ink #0b0b0c, Paper #f4f1ea, Phosphor Gold
 * #ffb000, muted #9b9890 (5.9:1 on ink). Keep the markup inside Satori's
 * supported subset — plain <path>/<rect> fills only, no gradients, masks, or
 * filters (the trails are pre-flattened rounded rects for exactly this
 * reason).
 */
export const alt =
  "stella — the terminal agent. faster · cheaper · more accurate";
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
          background: "#0b0b0c", // ink
          color: "#f4f1ea", // paper
          padding: "80px",
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: "16px",
            fontSize: "26px",
            letterSpacing: "0.14em",
            textTransform: "lowercase",
            color: "#9b9890",
          }}
        >
          <div style={{ width: "16px", height: "28px", background: GOLD }} />
          <span>the terminal agent</span>
        </div>

        <div style={{ display: "flex", flexDirection: "column" }}>
          <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
            <svg viewBox="0 0 96 96" width={170} height={170}>
              {TRAIL_RECTS.map((r) => (
                <rect key={r.y} {...r} fill={GOLD} />
              ))}
              <path d={STAR_PATH} fill={GOLD} />
            </svg>
            <svg viewBox={WORDMARK_VIEW_BOX} width={470} height={171}>
              <path d={WORDMARK_LETTERS_PATH} fill="#f4f1ea" />
              <path d={WORDMARK_SPARKLE_PATH} fill={GOLD} />
            </svg>
          </div>
          <div
            style={{
              display: "flex",
              marginTop: "28px",
              fontSize: "40px",
              color: "#9b9890",
              maxWidth: "980px",
              lineHeight: 1.3,
            }}
          >
            faster · cheaper · more accurate — a terminal coding agent that
            proves its work finished.
          </div>
        </div>

        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            fontSize: "26px",
            color: "#8d8a82",
          }}
        >
          <div style={{ display: "flex" }}>stella.oxagen.sh</div>
          <div style={{ display: "flex" }}>BYOK · model-agnostic · local-first</div>
        </div>
      </div>
    ),
    { ...size },
  );
}
