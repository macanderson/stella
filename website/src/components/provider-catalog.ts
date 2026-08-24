/**
 * The provider catalog — the single place the per-provider facts live. They
 * used to be repeated in a five-column table on the API Providers index *and*
 * again in prose on `getting-started/providers`, which is how the two drifted:
 * a table cell is invisible to every check the repo runs, so a renamed env var
 * is only ever found by a reader who tries it and fails. One typed record,
 * three call sites now:
 *
 *  - `provider-cards.tsx` renders it as the card grid;
 *  - `src/lib/page-markdown.ts` renders it as a markdown table when a docs
 *    page is exported (Copy page / llms.mdx), where the cards cannot go.
 *
 * It lives in a plain `.ts` module — not beside the components — because the
 * markdown path is exercised by `node --test`, which strips types but cannot
 * parse JSX.
 *
 * The wire `dialect` is worth carrying per provider: it is what "vendor-
 * agnostic" actually cashes out to. Stella speaks each vendor's own protocol
 * rather than normalizing everything through one OpenAI-shaped adapter, so the
 * dialect tells you which provider quirks (thinking blocks, cache-control,
 * tool-call shapes) are native rather than emulated.
 */

export interface ProviderSpec {
  /** Registry id — the `provider` half of `--model provider/model`. */
  id: string;
  name: string;
  /**
   * Deep link into the single API Providers page. These were ten separate
   * pages until the provider docs were consolidated; the anchors are the
   * heading ids that page actually emits, so a reworded heading breaks a link
   * here and must be re-checked against the rendered HTML, not guessed from
   * the heading text.
   */
  href: string;
  /** One line on when you would pick this provider over the others. */
  blurb: string;
  /** The primary credential variable; aliases in parentheses. */
  env: string;
  defaultModel: string;
  dialect: string;
}

export const PROVIDER_CATALOG: ProviderSpec[] = [
  {
    id: "anthropic",
    name: "Anthropic",
    href: "/docs/api-providers#anthropic",
    blurb: "The strongest coding and agentic models in the catalog, with first-class prompt caching.",
    env: "ANTHROPIC_API_KEY",
    defaultModel: "claude-fable-5",
    dialect: "Anthropic Messages",
  },
  {
    id: "openai",
    name: "OpenAI",
    href: "/docs/api-providers#openai",
    blurb: "A strong worker and the usual second family for cross-family judging.",
    env: "OPENAI_API_KEY",
    defaultModel: "gpt-5.5",
    dialect: "OpenAI Responses",
  },
  {
    id: "gemini",
    name: "Google Gemini",
    href: "/docs/api-providers#google-gemini",
    blurb: "Very large context windows at a low price — the long-document worker.",
    env: "GEMINI_API_KEY (GOOGLE_API_KEY)",
    defaultModel: "gemini-3-pro",
    dialect: "Gemini generateContent",
  },
  {
    id: "vertex",
    name: "Google Vertex AI",
    href: "/docs/api-providers#google-vertex-ai",
    blurb: "The same Gemini models billed through your GCP project, for enterprises that require it.",
    env: "VERTEX_ACCESS_TOKEN",
    defaultModel: "gemini-3-pro",
    dialect: "Gemini via Vertex",
  },
  {
    id: "bedrock",
    name: "Amazon Bedrock",
    href: "/docs/api-providers#amazon-bedrock",
    blurb: "Claude and friends inside your AWS account, on your existing IAM and billing.",
    env: "AWS_ACCESS_KEY_ID",
    defaultModel: "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
    dialect: "Bedrock Converse",
  },
  {
    id: "xai",
    name: "xAI",
    href: "/docs/api-providers#xai",
    blurb: "Grok, over the OpenAI-compatible dialect.",
    env: "XAI_API_KEY",
    defaultModel: "grok-4",
    dialect: "OpenAI-compatible",
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    href: "/docs/api-providers#deepseek",
    blurb: "Very cheap per token — the reference budget worker.",
    env: "DEEPSEEK_API_KEY",
    defaultModel: "deepseek-chat",
    dialect: "OpenAI-compatible",
  },
  {
    id: "zai",
    name: "Z.ai",
    href: "/docs/api-providers#zai",
    blurb: "GLM models, and a flat-rate coding plan that decouples cost from token count.",
    env: "ZAI_API_KEY",
    defaultModel: "glm-5.2",
    dialect: "OpenAI-compatible",
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    href: "/docs/api-providers#openrouter",
    blurb: "One key, hundreds of models — the gateway when you would rather not manage keys.",
    env: "OPENROUTER_API_KEY",
    defaultModel: "moonshotai/kimi-k3",
    dialect: "OpenAI-compatible",
  },
  {
    id: "local",
    name: "Local server",
    href: "/docs/api-providers#local-servers",
    blurb: "Ollama, llama.cpp, vLLM, LM Studio — anything that serves the OpenAI shape. No key, no egress.",
    env: "none (optional LOCAL_API_KEY)",
    defaultModel: "you choose",
    dialect: "OpenAI-compatible",
  },
];
