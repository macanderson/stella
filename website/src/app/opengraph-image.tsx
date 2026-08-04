import { ImageResponse } from "next/og";
import {
  GOLD,
  STAR_PATH,
  TRAIL_RECTS,
  WORDMARK_LETTERS_PATH,
} from "@/components/brand";

/**
 * The social card, generated at build time (next/og) so it stays in sync with
 * the brand and ships no static binary.
 *
 * It is the lockup on Ink — the gold comet flying left→right into the outlined
 * wordmark — over the same composition the kit's banners carry in
 * docs/brand/social/: the Homebrew line in a terminal, the repo named, and the
 * ask to star it. The two are meant to be recognisably one card, so a change
 * here usually wants `python3 docs/brand/social/build_social.py` run too.
 *
 * The wordmark drops its sparkle here (viewBox 228 rather than 264). The comet
 * is already sitting beside it, and brand.tsx's rule is that the two never
 * double up — the kit's own lockup SVG omits it for the same reason.
 *
 * Colours are literals or imported constants rather than CSS vars because
 * Satori resolves no cascade: Ink #0b0b0c, Paper #f4f1ea, Phosphor Gold
 * #ffb000, muted #9b9890 (5.9:1 on ink). Keep the markup inside Satori's
 * supported subset — plain <path>/<rect> fills only, no gradients, masks, or
 * filters (the trails are pre-flattened rounded rects for exactly this
 * reason), and every element with children carries an explicit `display`.
 *
 * The install line is NOT monospaced, and that is a constraint rather than a
 * choice: Satori decodes ttf/otf/woff and the brand kit ships JetBrains Mono
 * as woff2 only, so asking for the face here would silently fall back. The
 * terminal therefore has to read as a terminal from its chrome — title bar,
 * dots, prompt, resting cursor — which is why those are drawn rather than
 * implied. The kit's PNGs render through librsvg and do get the real face.
 */
export const alt =
  "stella — the terminal agent. brew install macanderson/tap/stella · star macanderson/stella on GitHub";
export const size = { width: 1200, height: 630 };
export const contentType = "image/png";

const INK = "#0b0b0c";
const PAPER = "#f4f1ea";
const MUTED = "#9b9890";
const SURFACE = "#121214";
const SURFACE_TOP = "#17171a";
const BORDER = "#2b2b2e";

/** The repo this card advertises, and the one command that installs it. */
const REPO_SLUG = "macanderson/stella";
const INSTALL_CMD = "brew install macanderson/tap/stella";

/** The GitHub mark — same path as `GitHubMark`, inlined for Satori's subset. */
const GITHUB_PATH =
  "M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12";

function Comet({ size: px }: { size: number }) {
  return (
    <svg viewBox="0 0 96 96" width={px} height={px}>
      {TRAIL_RECTS.map((r) => (
        <rect key={r.y} {...r} fill={GOLD} />
      ))}
      <path d={STAR_PATH} fill={GOLD} />
    </svg>
  );
}

/**
 * The comet star on its own — the icon on the star-the-repo pill.
 *
 * The viewBox is cropped to the star's ink (42,26)–(86,70) rather than reusing
 * the mark's 96-unit box, where the star fills under half the width and would
 * render at less than half the size asked for.
 */
function StarGlyph({ size: px }: { size: number }) {
  return (
    <svg viewBox="42 26 44 44" width={px} height={px}>
      <path d={STAR_PATH} fill={GOLD} />
    </svg>
  );
}

/**
 * The two corner sweeps: the same comet star past canvas scale, one receding
 * and one warm, placed so only the concave waist between two arms crosses the
 * frame. Geometry matches `sweep()` in docs/brand/social/build_social.py — the
 * transforms are pre-solved here because Satori has no layout for SVG.
 */
function Sweeps() {
  return (
    <svg
      width={size.width}
      height={size.height}
      viewBox={`0 0 ${size.width} ${size.height}`}
      style={{ position: "absolute", top: 0, left: 0 }}
    >
      <path
        d={STAR_PATH}
        transform="translate(-3818.6 -2755.1) scale(54.5455)"
        fill="#000000"
        fillOpacity={0.38}
      />
      <path
        d={STAR_PATH}
        transform="translate(-1806.9 -1738.6) scale(51.8182)"
        fill={GOLD}
        fillOpacity={0.055}
      />
    </svg>
  );
}

export default function OpengraphImage() {
  return new ImageResponse(
    (
      <div
        style={{
          width: "100%",
          height: "100%",
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          background: INK,
          color: PAPER,
          position: "relative",
          // No padding here on purpose: an absolutely-positioned child anchors
          // to the padding box, so padding would inset the sweeps off-canvas.
          // The stack is 820px inside 1200px and centres itself.
        }}
      >
        <Sweeps />

        {/* The lockup: comet, then the name. */}
        <div style={{ display: "flex", alignItems: "center" }}>
          <Comet size={118} />
          <svg viewBox="0 0 228 96" width={280} height={118}>
            <path d={WORDMARK_LETTERS_PATH} fill={PAPER} />
          </svg>
        </div>

        <div style={{ display: "flex", marginTop: "18px", fontSize: "27px", color: MUTED }}>
          the terminal agent — faster · cheaper · more accurate
        </div>

        {/* A picture of a terminal. Nothing here copies — it is read and retyped. */}
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            marginTop: "42px",
            width: "820px",
            borderRadius: "16px",
            border: `2px solid ${BORDER}`,
            background: SURFACE,
          }}
        >
          <div
            style={{
              display: "flex",
              alignItems: "center",
              height: "56px",
              padding: "0 22px",
              background: SURFACE_TOP,
              borderBottom: `2px solid ${BORDER}`,
              borderTopLeftRadius: "14px",
              borderTopRightRadius: "14px",
            }}
          >
            {/* One dot gold: gold is the signal, so exactly one thing gets it. */}
            {[GOLD, "#4a4945", "#35342f"].map((c, i) => (
              <div
                key={c}
                style={{
                  width: "14px",
                  height: "14px",
                  borderRadius: "7px",
                  background: c,
                  marginLeft: i === 0 ? 0 : "10px",
                }}
              />
            ))}
            <div
              style={{
                display: "flex",
                flexGrow: 1,
                justifyContent: "center",
                fontSize: "22px",
                color: MUTED,
                // Balance the dots so the title sits on the box's centre line.
                marginRight: "62px",
              }}
            >
              {REPO_SLUG}
            </div>
          </div>

          <div
            style={{
              display: "flex",
              alignItems: "center",
              height: "84px",
              padding: "0 26px",
              fontSize: "32px",
            }}
          >
            <div style={{ display: "flex", color: GOLD }}>$</div>
            <div style={{ display: "flex", marginLeft: "14px" }}>{INSTALL_CMD}</div>
            {/* A resting block cursor — the tell that says terminal, not code block. */}
            <div
              style={{
                width: "17px",
                height: "32px",
                marginLeft: "12px",
                background: GOLD,
              }}
            />
          </div>
        </div>

        {/* The repo, then the ask — spanning the terminal's own width. */}
        <div
          style={{
            display: "flex",
            width: "820px",
            marginTop: "34px",
            alignItems: "center",
            justifyContent: "space-between",
            fontSize: "28px",
          }}
        >
          <div style={{ display: "flex", alignItems: "center" }}>
            <svg viewBox="0 0 24 24" width={30} height={30}>
              <path d={GITHUB_PATH} fill={PAPER} />
            </svg>
            <div style={{ display: "flex", marginLeft: "14px" }}>{REPO_SLUG}</div>
          </div>

          <div
            style={{
              display: "flex",
              alignItems: "center",
              padding: "12px 26px",
              borderRadius: "34px",
              border: `2px solid ${GOLD}`,
              // The one thing on the card a reader is meant to act on.
              background: "rgba(255,176,0,0.12)",
              color: GOLD,
            }}
          >
            <StarGlyph size={30} />
            <div style={{ display: "flex", marginLeft: "12px" }}>star the repo</div>
          </div>
        </div>
      </div>
    ),
    { ...size },
  );
}
