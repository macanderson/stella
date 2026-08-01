import { Callout as FumadocsCallout } from "fumadocs-ui/components/callout";
import defaultMdxComponents from "fumadocs-ui/mdx";
import type { MDXComponents } from "mdx/types";
import type { ComponentProps } from "react";

import { Badge, CardGrid, OptionCard, SpecCard, ToolCard } from "@/components/cards";
import { DocsCodeBlock } from "@/components/docs-code-block";
import {
  BudgetGuardDiagram,
  ClaimLockDiagram,
  CostChainDiagram,
  CredentialChainDiagram,
  EngineGateDiagram,
  EngineOwnershipDiagram,
  EngineSequenceDiagram,
  EngineTestHarnessDiagram,
  EventContractDiagram,
  FleetFanoutDiagram,
  HeroFlowDiagram,
  HookLifecycleDiagram,
  LoopVerdictDiagram,
  McpTopologyDiagram,
  PermissionGateDiagram,
  PipelineFlowDiagram,
  QuickstartDiagram,
  RecallLoopDiagram,
  SettingsCascadeDiagram,
  SingleThreadDiagram,
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
    // Code blocks are terminals: always-ink, click-anywhere copy, floating
    // "copied!" chip — the home page's install-block handling, docs-wide.
    pre: (props) => <DocsCodeBlock {...props} />,
    // Callouts carry their accent as the card's own left border rather than as
    // an inset spacer div, so the coloured edge sits flush and follows the
    // corner radius. The class is the hook; the rule lives in global.css.
    Callout: ({ className, ...props }: ComponentProps<typeof FumadocsCallout>) => (
      <FumadocsCallout
        className={className ? `stella-callout ${className}` : "stella-callout"}
        {...props}
      />
    ),
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
    EngineOwnershipDiagram,
    EngineTestHarnessDiagram,
    EngineGateDiagram,
    EngineSequenceDiagram,
    LoopVerdictDiagram,
    McpTopologyDiagram,
    HookLifecycleDiagram,
    SingleThreadDiagram,
    EventContractDiagram,
    BudgetGuardDiagram,
    CostChainDiagram,
    ClaimLockDiagram,
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
