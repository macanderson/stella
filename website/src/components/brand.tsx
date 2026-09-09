/**
 * The stella marks — the Oxagen house brand system.
 *
 * Geometry comes from `./brand-marks.generated.ts`, extracted by
 * `scripts/sync-brand-assets.mjs` from the house kit
 * (macanderson/oxagen-house-brand). Nothing here is drawn: the wordmark is
 * Space Grotesk's own outlines at the kit's logo weight, which is the face this
 * site is set in, so the name in the nav and the name in a sentence are the same
 * design. To change a mark, change the kit and re-run the sync.
 *
 * ## The comet is retired
 *
 * v5.0's mark was a four-point star with a trail, drawn by
 * `docs/brand/cometkit.py`. The house system retires it along with Oxagen's own
 * o+cursor mark. Stella's mark is the ASTERISK, and it is already inside the
 * word — `stella*` — so:
 *
 *  - **The wordmark IS the lockup.** There is no separate one and nothing is
 *    ever placed to the left of the word. `sparkle={false}` is gone with the
 *    comet: it existed so the star would not double up beside a mark that no
 *    longer exists, and dropping the asterisk now would remove the only mark
 *    the word has.
 *  - **The icon is the asterisk alone.** Unlike Oxagen's one-colour `Ox`
 *    lettermark it ships GOLD, because a lone asterisk in ink reads as
 *    punctuation rather than as a mark. That is the kit's own exception, not
 *    this file's.
 *  - **One glyph is gold, never two.** The asterisk in the word; nothing else.
 *    It keeps the metal in both themes — the kit's "gold becomes its deep shade
 *    on paper" rule governs gold WORDS, not the mark, and the kit's own light
 *    and dark files both fill the accent with the metal.
 *
 * The letters take `currentColor`, so the name inverts with the theme.
 */

import {
  BRAND_GOLD,
  MARK_PATH,
  MARK_TRANSFORM,
  MARK_VIEW_BOX,
  WORDMARK_LETTERS_PATH,
  WORDMARK_SPARKLE_PATH,
  WORDMARK_VIEW_BOX,
} from "./brand-marks.generated";

export {
  BRAND_GOLD,
  MARK_PATH,
  MARK_TRANSFORM,
  MARK_VIEW_BOX,
  WORDMARK_LETTERS_PATH,
  WORDMARK_SPARKLE_PATH,
  WORDMARK_VIEW_BOX,
};

/**
 * Gold, as an inline literal for contexts with no cascade (Satori renders the
 * OG card with no stylesheet). Prefer the token everywhere a cascade exists.
 */
export const BRAND = BRAND_GOLD;

/**
 * The GitHub mark. Used wherever the site links out to the repo — the nav,
 * every doc page's footer — so it lives once rather than as a pasted path in
 * each place that needs it. Decorative by default, same rule as `Mark`: pass
 * a `label` where the icon is the link's only content and needs its own
 * accessible name.
 */
export function GitHubMark({
  className,
  label,
}: {
  className?: string;
  label?: string;
}) {
  const a11y = label
    ? { role: "img", "aria-label": label }
    : { "aria-hidden": true };
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" className={className} {...a11y}>
      <path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" />
    </svg>
  );
}

/**
 * The asterisk on its own. Decorative by default (`aria-hidden`) because every
 * place it appears is beside the name in text; pass a `label` where it stands
 * alone.
 *
 * Square slots only — a favicon, an avatar, a tight piece of chrome. Setting it
 * beside the wordmark would put two asterisks on one line, since the word
 * already carries one.
 */
export function Mark({
  className,
  label,
}: {
  className?: string;
  label?: string;
}) {
  const a11y = label
    ? { role: "img", "aria-label": label }
    : { "aria-hidden": true };
  return (
    <svg viewBox={MARK_VIEW_BOX} className={className} {...a11y}>
      <g transform={MARK_TRANSFORM}>
        <path d={MARK_PATH} fill={`var(--stella-mark-shape, ${BRAND})`} />
      </g>
    </svg>
  );
}

/**
 * THE stella logo — `stella*`, letters in `currentColor`, the asterisk in gold.
 *
 * This is the lockup; there is no other. The asterisk is not optional, which is
 * why the old `sparkle` prop is gone: it existed so the star would not double
 * up beside the retired comet, and without it the word carries no mark at all.
 */
export function Wordmark({ className }: { className?: string }) {
  return (
    <svg
      viewBox={WORDMARK_VIEW_BOX}
      fill="currentColor"
      className={className}
      role="img"
      aria-label="stella"
    >
      <path d={WORDMARK_LETTERS_PATH} />
      <path
        d={WORDMARK_SPARKLE_PATH}
        fill={`var(--stella-mark-shape, ${BRAND})`}
      />
    </svg>
  );
}
