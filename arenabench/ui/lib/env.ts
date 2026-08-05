// Client-side env parsing exists only to reassure the operator that the key
// they think they pasted is actually there. It mirrors the server's parser
// for the common cases; the server's result is authoritative.

export function parseEnvKeys(text: string): string[] {
  const keys: string[] = [];
  for (let line of String(text).split("\n")) {
    line = line.trim();
    if (!line || line.startsWith("#")) continue;
    if (line.startsWith("export ")) line = line.slice(7).trim();
    const eq = line.indexOf("=");
    if (eq < 1) continue;
    const key = line.slice(0, eq).trim();
    if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)) keys.push(key);
  }
  return keys;
}

export const CREDENTIALS: Record<string, string[]> = {
  openrouter: ["OPENROUTER_API_KEY"],
  anthropic: ["ANTHROPIC_API_KEY"],
  openai: ["OPENAI_API_KEY"],
  zai: ["ZAI_API_KEY"],
  google: ["GEMINI_API_KEY", "GOOGLE_API_KEY"],
  deepseek: ["DEEPSEEK_API_KEY"],
  moonshot: ["MOONSHOT_API_KEY"],
  xai: ["XAI_API_KEY"],
  mistral: ["MISTRAL_API_KEY"],
};

export type EnvTone = "ok" | "warn";

export function envStatus(env: string, apiName: string): { tone: EnvTone; text: string } {
  const keys = parseEnvKeys(env);
  const wanted = CREDENTIALS[(apiName || "").toLowerCase()] || [];
  const has = wanted.some((k) => keys.includes(k));
  if (!keys.length) {
    return { tone: "warn", text: "no variables — this seat has no credential" };
  }
  if (wanted.length && !has) {
    return { tone: "warn", text: `${keys.length} vars, but none of: ${wanted.join(" / ")}` };
  }
  return {
    tone: "ok",
    text: `${keys.length} vars · ${keys.slice(0, 6).join(" ")}${keys.length > 6 ? " …" : ""}`,
  };
}
