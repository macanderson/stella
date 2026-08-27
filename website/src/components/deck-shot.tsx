import { DECK_SHOTS, type DeckShotId } from "./deck-shots";

/**
 * One command-deck rendering, framed.
 *
 * The SVG draws its own terminal — rounded ink panel, hairline border, the
 * deck's own palette — so this adds a shadow and a caption and otherwise gets
 * out of the way. Wrapping it in a second bordered card would double the
 * border and make the frame look like a screenshot of a screenshot.
 *
 * A plain `<img>` rather than `next/image`: the site sets
 * `images: { unoptimized: true }` (it is fully static), so the component would
 * add a wrapper and a srcset over a vector that needs neither. The CSP allows
 * `img-src 'self'`, which is where these are served from.
 *
 * `width`/`height` come from the record and are the SVG's own viewBox, so the
 * browser reserves the right box before the file lands and the page does not
 * jump. The CSS overrides both — the frame scales to its column.
 */
export function DeckShot({
  id,
  caption,
  priority = false,
}: {
  id: DeckShotId;
  /** Overrides the record's caption. Pass `null` for a bare frame. */
  caption?: string | null;
  /** Set on a shot above the fold, so it is not lazily loaded. */
  priority?: boolean;
}) {
  const shot = DECK_SHOTS[id];
  const text = caption === undefined ? shot.caption : caption;
  return (
    <figure className="deck-shot">
      <img
        className="deck-shot-img"
        src={`/tui/${shot.file}.svg`}
        alt={shot.alt}
        width={shot.width}
        height={shot.height}
        loading={priority ? "eager" : "lazy"}
        decoding="async"
        fetchPriority={priority ? "high" : "auto"}
      />
      {text ? (
        <figcaption className="deck-shot-cap">
          <span className="deck-shot-tag">stella</span>
          {text}
        </figcaption>
      ) : null}
    </figure>
  );
}
