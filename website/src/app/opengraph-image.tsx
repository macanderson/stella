import { ImageResponse } from "next/og";
import {
  BRAND_GOLD,
  MARK_BOX_FLAT,
  MARK_PATH_FLAT,
  WORDMARK_LETTERS_PATH,
  WORDMARK_SPARKLE_PATH,
  WORDMARK_VIEW_BOX,
} from "@/components/brand-marks.generated";

/**
 * The social card, generated at build time (next/og) so it stays in sync with
 * the brand and ships no static binary.
 *
 * The identity is `stella*` on Ink — the WORDMARK alone, which under the house
 * system is the whole lockup: the asterisk is already the mark and it lives
 * inside the word, so nothing is set to its left. The retired comet used to fly
 * in from there, and the wordmark dropped its sparkle to avoid two marks on one
 * line; both of those went with the comet.
 *
 * Around it is the composition the kit's own banners carry in
 * docs/brand/social/: the Homebrew line in a terminal, the repo named, and the
 * ask to star it.
 *
 * The corner sweeps are the asterisk at canvas scale, bled off-frame. That is
 * the kit's own rule for a surface that needs a picture — build one out of the
 * icon: its outline, a field of it, or simply a much bigger one. No stock
 * illustration, no gradient mesh.
 *
 * Colours are literals or imported constants rather than CSS vars because
 * Satori resolves no cascade: the canvas #10100f, text #f2eee5, gold #d6962c
 * (7.5:1 on ink), muted #8c877c (5.0:1 on ink). Keep the markup inside Satori's
 * supported subset — plain <path>/<rect> fills only, no gradients, masks, or
 * filters — and every element with children carries an explicit `display`.
 * `MARK_PATH_FLAT` exists for that subset: it is the asterisk with its placing
 * transform already solved into the coordinates, so it needs no enclosing <g>.
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

const INK = "#10100f";
const PAPER = "#f2eee5";
const MUTED = "#8c877c";
const SURFACE = "#181715";
const SURFACE_TOP = "#201f1c";
const BORDER = "#292722";

/** The repo this card advertises, and the one command that installs it. */
const REPO_SLUG = "macanderson/stella";
const INSTALL_CMD = "brew install macanderson/tap/stella";

/** The GitHub mark — same path as `GitHubMark`, inlined for Satori's subset. */
const GITHUB_PATH =
  "M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12";

/**
 * The asterisk on its own — the icon on the star-the-repo pill.
 *
 * Cropped to `MARK_BOX_FLAT`, the mark's own ink, rather than the kit's 96-unit
 * box: inside that box the glyph fills 60% of the width and would render at
 * 60% of the size asked for.
 */
function StarGlyph({ size: px }: { size: number }) {
  return (
    <svg viewBox={MARK_BOX_FLAT} width={px} height={px}>
      <path d={MARK_PATH_FLAT} fill={BRAND_GOLD} />
    </svg>
  );
}

/**
 * The two corner sweeps: the asterisk past canvas scale, one receding and one
 * warm, placed so only the concave waist between two arms crosses the frame.
 *
 * The transforms are pre-solved because Satori has no layout for SVG. They were
 * re-solved for the house mark rather than carried over: `MARK_PATH_FLAT` is
 * centred on (48, 48) in its own box where the comet's star was centred on
 * (64, 48), so reusing the old numbers would have slid both sweeps 16 units ×
 * the scale — roughly 870px and 830px — off frame.
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
        d={MARK_PATH_FLAT}
        transform="translate(-4867.8 -3803.2) scale(54.5455)"
        fill="#000000"
        fillOpacity={0.38}
      />
      <path
        d={MARK_PATH_FLAT}
        transform="translate(-2801.9 -2733.6) scale(51.8182)"
        fill={BRAND_GOLD}
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

        {/* The lockup, which is the wordmark: `stella*`, its asterisk in gold. */}
        <div style={{ display: "flex", alignItems: "center" }}>
          <svg viewBox={WORDMARK_VIEW_BOX} width={379} height={88}>
            <path d={WORDMARK_LETTERS_PATH} fill={PAPER} />
            <path d={WORDMARK_SPARKLE_PATH} fill={BRAND_GOLD} />
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
            {[BRAND_GOLD, "#504c44", "#34322d"].map((c, i) => (
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
            <div style={{ display: "flex", color: BRAND_GOLD }}>$</div>
            <div style={{ display: "flex", marginLeft: "14px" }}>{INSTALL_CMD}</div>
            {/* A resting block cursor — the tell that says terminal, not code block. */}
            <div
              style={{
                width: "17px",
                height: "32px",
                marginLeft: "12px",
                background: BRAND_GOLD,
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
              border: `2px solid ${BRAND_GOLD}`,
              // The one thing on the card a reader is meant to act on. The
              // wash is the accent at 12%, written out because Satori has no
              // cascade and cannot resolve a custom property.
              background: "rgba(214,150,44,0.12)",
              color: BRAND_GOLD,
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
