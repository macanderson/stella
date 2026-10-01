---
name: oxagen-branding
version: 1.0.0
scope: workspace
description: The authority for anything that carries Oxagen or Stella branding or speaks in Oxagen's voice. Use it whenever you create or edit a page, post, ad, email, deck, doc, spec, UI string, error message, CLI output, README, or any prose a person will read on behalf of either brand, even if the request does not say "brand" or "voice". Covers the marks, tokens, type, layout rules, positioning, one-liners, voice and tone, words to use, words to avoid, and worked examples. Oxagen and Stella share one house system; the logo is the only difference.
---

# Oxagen branding

This file is a stub. The skill and the brand it describes live in `oxageninc/brand` on `main`. Fetch the skill from there and follow it. Never write brand copy, colours, or type from memory.

```sh
sha=$(git ls-remote https://github.com/oxageninc/brand refs/heads/main | cut -f1)
curl -fsSL "https://raw.githubusercontent.com/oxageninc/brand/$sha/skills/oxagen-branding/SKILL.md"
```

Read every file the skill names from the same `$sha`. Use `curl` when you have a shell. A web-fetch tool may summarise what it reads, so without a shell ask it for the file verbatim.

If the network fails, use a local checkout (`$OXAGEN_BRAND_KIT`, else `~/Projects/oxagen-brand`): run `git -C <kit> fetch origin main`, then read each file with `git -C <kit> show origin/main:<path>`. If that fails too, stop and say that the brand source is unreachable.

This stub comes from `skills/stub/oxagen-branding/` in oxagen-brand, and `skills/install.sh` installs it. Do not edit it here.
