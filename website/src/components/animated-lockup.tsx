import {
  BRAND_GOLD,
  SWEEP,
  WORDMARK_LETTERS_PATH,
  WORDMARK_SPARKLE_PATH,
  WORDMARK_VIEW_BOX,
} from "@/components/brand-marks.generated";

/**
 * The house motion, played ONCE: `stella*` at rest, and the metal sweeping
 * across it a single time.
 *
 * It replaces the comet's typing lockup — trails arriving left→right, a star
 * popping, six letters typing on behind a gold cursor. That was choreography
 * for a mark the house system retired; the kit's own motion is this shimmer,
 * and it is the same animation on every surface the kit renders.
 *
 * Nothing here is authored. The geometry is the kit's wordmark and the timing
 * is `SWEEP`, read out of `spinners/stella-spinner-wordmark.svg` by
 * `scripts/sync-brand-assets.mjs`, so re-running the sync after a kit rebuild
 * re-times this component without anyone touching it. The kit loops; this runs
 * once and holds, because a landing page is read rather than watched, and
 * every track's final state is its resting state.
 *
 * Server component on purpose — the whole animation is CSS, so it costs no
 * client JavaScript and starts on first paint.
 */

/**
 * The one number the stylesheet has to spell literally: a keyframe selector is
 * not a place `var()` resolves, so `.lp-lk-sweep`'s hold stop is written as
 * `55%` in global.css. This makes the kit moving it a compile error here rather
 * than a shimmer that silently changes feel.
 */
const CSS_HOLD_PCT: typeof SWEEP.holdPct = 55;

/** Unique per render tree — two lockups on one page must not share a clip. */
const CLIP_ID = "lp-lk-letters";

export function AnimatedLockup({ className }: { className?: string }) {
  return (
    <svg
      viewBox={WORDMARK_VIEW_BOX}
      className={className}
      role="img"
      aria-label="stella"
      // The timing crosses into CSS as custom properties so the keyframes stay
      // in the stylesheet (the diagrams' one-stylesheet convention) while the
      // numbers stay generated.
      style={
        {
          "--lk-duration": `${SWEEP.durationSec}s`,
          "--lk-easing": SWEEP.easing,
          "--lk-skew": `${SWEEP.skewDeg}deg`,
          "--lk-travel": `${SWEEP.travelPx}px`,
        } as React.CSSProperties
      }
      data-hold-pct={CSS_HOLD_PCT}
    >
      <defs>
        {/* The shimmer is clipped to the letters, so the metal appears to move
            through the word rather than over it. */}
        <clipPath id={CLIP_ID}>
          <path d={WORDMARK_LETTERS_PATH} />
        </clipPath>
        <linearGradient id={`${CLIP_ID}-g`} x1="0" y1="0" x2="1" y2="0">
          <stop offset="0" stopColor={SWEEP.highlight} stopOpacity="0" />
          <stop offset="0.5" stopColor={SWEEP.highlight} stopOpacity="0.95" />
          <stop offset="1" stopColor={SWEEP.highlight} stopOpacity="0" />
        </linearGradient>
      </defs>
      {/* The word inverts with the theme; the asterisk keeps the metal. */}
      <path d={WORDMARK_LETTERS_PATH} fill="currentColor" />
      <path
        d={WORDMARK_SPARKLE_PATH}
        fill={`var(--stella-mark-shape, ${BRAND_GOLD})`}
      />
      <g clipPath={`url(#${CLIP_ID})`}>
        <rect
          className="lp-lk-sweep"
          x={SWEEP.x}
          y={SWEEP.y}
          width={SWEEP.width}
          height={SWEEP.height}
          fill={`url(#${CLIP_ID}-g)`}
          transform={`skewX(${SWEEP.skewDeg})`}
        />
      </g>
    </svg>
  );
}
