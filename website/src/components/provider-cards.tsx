import { CardGrid, SpecCard } from "@/components/cards";
import { ProviderLogo } from "@/components/provider-logos";
import { PROVIDER_CATALOG, type ProviderSpec } from "@/components/provider-catalog";

/**
 * The provider catalog, rendered as cards. The facts live in
 * `provider-catalog.ts`; this file is only the rendering.
 *
 * The card shows the provider's name as text beside its logomark rather than
 * as a wordmark lockup. The name is then selectable, searchable, and indexed;
 * a wordmark is none of those, and ten of them side by side turned the grid
 * into a logo wall.
 */

/**
 * Every supported provider as a card grid.
 *
 * `only` narrows the set for pages that discuss a subset (the getting-started
 * walkthrough shows the three single-key providers, not all ten), keeping those
 * pages sourced from this same record instead of re-typing the facts.
 */
export function ProviderGrid({ only }: { only?: string[] }) {
  const providers = only
    ? (only.map((id) => PROVIDER_CATALOG.find((p) => p.id === id)).filter(Boolean) as ProviderSpec[])
    : PROVIDER_CATALOG;

  return (
    <CardGrid size="md">
      {providers.map((p) => (
        <SpecCard
          key={p.id}
          href={p.href}
          title={<ProviderLogo id={p.id} size={20} />}
          meta={[
            { label: "id", value: p.id },
            { label: "Env var", value: p.env },
            { label: "Default", value: p.defaultModel },
            { label: "Dialect", value: p.dialect, mono: false },
          ]}
        >
          {p.blurb}
        </SpecCard>
      ))}
    </CardGrid>
  );
}
