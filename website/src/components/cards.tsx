import Link from "next/link";
import type { ReactNode } from "react";

/**
 * Card primitives for the docs — the mobile-first replacement for reference
 * tables.
 *
 * Why cards at all: a five-column table is a fine desktop artifact and a bad
 * phone one. It either overflows the viewport or squeezes every column to two
 * words, and on a narrow screen the reader loses the header row long before
 * they reach the cell that mattered. A card carries its own labels, so it reads
 * the same at 380px and 1400px — and it stacks instead of scrolling sideways.
 *
 * What a card is NOT: a panel. There is no fill, no radius, no shadow, and no
 * border box — a card is a rule and a heading, and the grid is a list with
 * seams. Grouping is carried by the markup (`<dl>`/`<dt>`/`<dd>`, one title per
 * card), so the seam is decoration and never the only cue.
 *
 * Three shapes, all built on the same `sc-grid`:
 * - `SpecCard`   — a title, optional badge, prose, and a label/value list.
 * - `ToolCard`   — tool name in mono, a read-only/mutating badge, one line of
 *   prose, and a left rule that repeats the badge as shape.
 * - `OptionCard` — one CLI flag or settings key, with its default in its own
 *   slot.
 *
 * Styles live in `src/app/global.css` under the `sc-` (stella card) prefix
 * rather than in a `<style>` tag here, so a page rendering forty cards emits
 * one stylesheet instead of forty. Every colour is a Fumadocs or brand token —
 * no hex literals — so all three invert with the theme.
 */

/* ───────────────────────────────────────────────────────────────────────────
 * Grid
 * ─────────────────────────────────────────────────────────────────────────── */

type GridSize = "sm" | "md" | "lg";

/**
 * Responsive card grid. `size` sets the *minimum* column width, and the grid
 * fits as many as the container allows — so the column count falls out of the
 * viewport instead of being hardcoded per breakpoint. Below the minimum it is
 * a single stacked column, which is the phone case.
 */
export function CardGrid({
  children,
  size = "md",
}: {
  children: ReactNode;
  size?: GridSize;
}) {
  return <div className={`sc-grid sc-grid-${size}`}>{children}</div>;
}

/* ───────────────────────────────────────────────────────────────────────────
 * Shared pieces
 * ─────────────────────────────────────────────────────────────────────────── */

/**
 * A label/value row inside a card. Mono is the default because these values are
 * overwhelmingly identifiers you will type or paste — env vars, model ids, file
 * paths — and a proportional face makes `l`/`1` and `O`/`0` ambiguous in
 * exactly the strings where that costs you a failed command.
 */
export interface CardMeta {
  label: string;
  value: ReactNode;
  /** Set false for prose values (a wire-dialect name, a sentence). */
  mono?: boolean;
}

function MetaList({ items }: { items: CardMeta[] }) {
  if (items.length === 0) return null;
  return (
    <dl className="sc-meta">
      {items.map((item) => (
        <div className="sc-meta-row" key={item.label}>
          <dt className="sc-meta-label">{item.label}</dt>
          <dd className="sc-meta-value" data-prose={item.mono === false}>
            {item.value}
          </dd>
        </div>
      ))}
    </dl>
  );
}

/**
 * Status badge — uppercase micro-type, no pill and no fill. `tone` is semantic,
 * not decorative: `mutating` and `caution` borrow the functional amber the
 * callouts use, because "this tool can change your files" is a warning, not a
 * label. The word always spells it out, so hue is never the sole cue, and
 * everything else stays neutral so the amber keeps meaning something.
 */
export function Badge({
  children,
  tone = "neutral",
}: {
  children: ReactNode;
  tone?: "neutral" | "read" | "mutating" | "caution";
}) {
  return (
    <span className="sc-badge" data-tone={tone}>
      {children}
    </span>
  );
}

/* ───────────────────────────────────────────────────────────────────────────
 * Cards
 * ─────────────────────────────────────────────────────────────────────────── */

/**
 * Generic card. When `href` is set the whole card is the link target — a
 * 300x160px tap target instead of a 12-character one, which is the difference
 * between usable and not on a phone.
 */
export function SpecCard({
  title,
  href,
  badge,
  logo,
  children,
  meta = [],
}: {
  /**
   * Optional: a provider card's identity is its logo lockup, which already
   * contains the wordmark. Repeating the name as text under it is noise, and
   * the lockup carries its own `aria-label`, so nothing is lost.
   */
  title?: ReactNode;
  href?: string;
  badge?: ReactNode;
  /** The card's visual identity — a provider lockup, an icon. */
  logo?: ReactNode;
  children?: ReactNode;
  meta?: CardMeta[];
}) {
  const body = (
    <>
      {logo ? <div className="sc-card-logo">{logo}</div> : null}
      {title || badge ? (
        <div className="sc-card-head">
          {title ? <span className="sc-card-title">{title}</span> : <span />}
          {badge}
        </div>
      ) : null}
      {children ? <div className="sc-card-body">{children}</div> : null}
      <MetaList items={meta} />
    </>
  );

  if (href) {
    return (
      <Link className="sc-card sc-card-link" href={href}>
        {body}
      </Link>
    );
  }
  return <div className="sc-card">{body}</div>;
}

/**
 * One CLI flag, subcommand, or settings key.
 *
 * Almost every command page carried a two-column `| Flag | Meaning |` table.
 * Two columns survive a phone better than five, but barely: the flag names are
 * long and unbreakable, so the first column either wraps mid-identifier or
 * shoves the meaning into a 12-character gutter. A card gives the name its own
 * line and the prose the full width.
 *
 * `defaultValue` is a distinct slot rather than a sentence in the description
 * because "what happens if I don't pass this" is the question a flag reference
 * is most often opened to answer, and it should not need reading a paragraph.
 */
export function OptionCard({
  name,
  defaultValue,
  required,
  children,
}: {
  name: string;
  defaultValue?: string;
  required?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="sc-card">
      <div className="sc-card-head">
        <code className="sc-tool-name">{name}</code>
        {required ? <Badge tone="caution">Required</Badge> : null}
      </div>
      <div className="sc-card-body">{children}</div>
      {defaultValue ? (
        <p className="sc-tool-when">
          <span className="sc-tool-when-label">Default</span>{" "}
          <code className="sc-mono">{defaultValue}</code>
        </p>
      ) : null}
    </div>
  );
}

/**
 * One tool in the built-in catalog. `access` drives both the badge and the
 * card's left rule, so the read-only/mutating split survives a squint — the
 * partition matters (read-only tools are speculative-eligible and permission
 * decisions hang off it), and it should not require reading a table column.
 */
export function ToolCard({
  name,
  access,
  children,
  when,
}: {
  name: string;
  access: "read" | "mutating";
  children: ReactNode;
  /** For conditionally-registered tools: what has to be true for it to exist. */
  when?: ReactNode;
}) {
  return (
    <div className="sc-card sc-tool" data-access={access}>
      <div className="sc-card-head">
        <code className="sc-tool-name">{name}</code>
        <Badge tone={access === "read" ? "read" : "mutating"}>
          {access === "read" ? "Read-only" : "Mutating"}
        </Badge>
      </div>
      <div className="sc-card-body">{children}</div>
      {when ? (
        <p className="sc-tool-when">
          <span className="sc-tool-when-label">Appears when</span> {when}
        </p>
      ) : null}
    </div>
  );
}
