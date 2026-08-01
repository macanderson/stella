import {
  GOLD,
  STAR_PATH,
  TRAIL_RECTS,
  WORDMARK_LETTERS_PATH,
} from "@/components/brand";

/**
 * The kit's animated lockup (docs/brand/brand-guidelines.html § motion),
 * played ONCE: the trails arrive left→right, the star pops, the letters type
 * on behind a gold cursor, the cursor blinks out. Same geometry, same
 * timeline, same easings as the kit's looping demo — the only difference is
 * `animation-iteration-count: 1` with `fill-mode: forwards`, which is safe
 * because every track's 100% keyframe is its resting state.
 *
 * Geometry is not duplicated: the six letters are the M-command segments of
 * WORDMARK_LETTERS_PATH, drawn inside the +100px group the kit uses for its
 * typing variant. Animation CSS lives in global.css (`lp-lk-*`), following
 * the diagrams' one-stylesheet convention.
 *
 * Server component on purpose — the whole animation is CSS, so it costs no
 * client JavaScript and starts on first paint.
 */

/** viewBox of the kit's typing lockup (`spin adap` variant). */
const VIEW_BOX = "0 0 352 96";

/**
 * One path per letter: s, t, e, l, l, a. Split on whitespace before `M` —
 * letter boundaries in the path string are `"Z M"`, while the counter
 * subpaths inside `e` and `a` are `"ZM"` with no gap and must stay in their
 * letter's path (a counter separated into its own path stops punching its
 * hole, and the extra pieces would fall outside the six animation slots).
 */
const LETTERS = WORDMARK_LETTERS_PATH.split(/\s+(?=M)/).filter(Boolean);

export function AnimatedLockup({ className }: { className?: string }) {
  return (
    <svg
      viewBox={VIEW_BOX}
      className={className}
      role="img"
      aria-label="stella"
    >
      <g>
        {TRAIL_RECTS.map((r, i) => (
          <rect
            key={r.y}
            {...r}
            className={`lp-lk-t${i}`}
            fill={`var(--stella-gold, ${GOLD})`}
          />
        ))}
        <path
          d={STAR_PATH}
          className="lp-lk-star"
          fill={`var(--stella-gold, ${GOLD})`}
        />
        <g transform="translate(100 0)" fill="currentColor">
          {LETTERS.map((d, i) => (
            <path key={d.slice(0, 12)} d={d} className={`lp-lk-l${i}`} />
          ))}
        </g>
        <rect
          className="lp-lk-cur"
          x="106"
          y="20.1"
          width="20"
          height="55.8"
          rx="2"
          fill={`var(--stella-gold, ${GOLD})`}
        />
      </g>
    </svg>
  );
}
