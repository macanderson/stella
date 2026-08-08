import fs from "node:fs";
import path from "node:path";
import { parse } from "smol-toml";

const HEX_RE = /^#[0-9a-fA-F]{6}$/;

function hexToRgb(hex) {
  if (!HEX_RE.test(hex)) {
    throw new Error(`brand-tokens.toml: "${hex}" is not a 6-digit #rrggbb color`);
  }
  return [
    parseInt(hex.slice(1, 3), 16),
    parseInt(hex.slice(3, 5), 16),
    parseInt(hex.slice(5, 7), 16),
  ];
}

function requireKey(table, key, tableName) {
  const value = table?.[key];
  if (typeof value !== "string") {
    throw new Error(`brand-tokens.toml: [${tableName}] is missing "${key}"`);
  }
  return value;
}

/**
 * Load and validate brand-tokens.toml, returning RGB triples ready for the
 * recolor engine. Throws with a specific, actionable message on any missing
 * or malformed key rather than letting a `undefined` propagate into a
 * silently-wrong recolor.
 */
export function loadTokens(tomlPath) {
  const raw = parse(fs.readFileSync(tomlPath, "utf8"));

  const nebulaHex = {
    violet: requireKey(raw.nebula, "violet", "nebula"),
    cyan: requireKey(raw.nebula, "cyan", "nebula"),
  };
  const darkHex = {
    bg: requireKey(raw.dark, "bg", "dark"),
    fg: requireKey(raw.dark, "fg", "dark"),
  };
  const lightHex = {
    bg: requireKey(raw.light, "bg", "light"),
    fg: requireKey(raw.light, "fg", "light"),
  };

  return {
    hex: { nebula: nebulaHex, dark: darkHex, light: lightHex },
    nebula: { violet: hexToRgb(nebulaHex.violet), cyan: hexToRgb(nebulaHex.cyan) },
    dark: { bg: hexToRgb(darkHex.bg), fg: hexToRgb(darkHex.fg) },
    light: { bg: hexToRgb(lightHex.bg), fg: hexToRgb(lightHex.fg) },
  };
}

export function defaultTokensPath(scriptDir) {
  return path.resolve(scriptDir, "../brand-tokens.toml");
}
