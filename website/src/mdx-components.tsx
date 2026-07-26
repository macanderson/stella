import defaultMdxComponents from "fumadocs-ui/mdx";
import type { MDXComponents } from "mdx/types";

import { Badge, CardGrid, OptionCard, SpecCard, ToolCard } from "@/components/cards";
import {
  CredentialChainDiagram,
  FleetFanoutDiagram,
  HeroFlowDiagram,
  PermissionGateDiagram,
  PipelineFlowDiagram,
  QuickstartDiagram,
  RecallLoopDiagram,
  SettingsCascadeDiagram,
  TelemetryFlowDiagram,
} from "@/components/diagrams";
import { ProviderGrid } from "@/components/provider-cards";
import { ProviderLogo, ProviderMark } from "@/components/provider-logos";

/**
 * MDX component map. The Fumadocs defaults (callouts, tabs, cards, code blocks
 * with copy buttons, headings) plus Stella's own bespoke pieces — registered
 * globally so any MDX page can use them without an import line.
 *
 * The card primitives are here for a specific reason: a wide reference table is
 * the docs' worst mobile surface, and every one that becomes a `CardGrid` is a
 * page that stops scrolling sideways on a phone.
 */
export function getMDXComponents(components?: MDXComponents): MDXComponents {
  return {
    ...defaultMdxComponents,
    // Diagrams
    HeroFlowDiagram,
    PipelineFlowDiagram,
    RecallLoopDiagram,
    FleetFanoutDiagram,
    QuickstartDiagram,
    CredentialChainDiagram,
    SettingsCascadeDiagram,
    PermissionGateDiagram,
    TelemetryFlowDiagram,
    // Cards — the mobile-first replacement for reference tables
    CardGrid,
    SpecCard,
    ToolCard,
    OptionCard,
    Badge,
    ProviderGrid,
    // Logos
    ProviderLogo,
    ProviderMark,
    ...components,
  };
}
