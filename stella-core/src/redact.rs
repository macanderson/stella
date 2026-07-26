//! Free-text secret redaction, applied before anything is persisted, indexed,
//! or embedded (spec §5.6).
//!
//! ## Why this exists
//!
//! There was no general-purpose text redactor in the workspace. The closest was
//! `stella-observatory`'s settings scrubber, which handles *structured* TOML and
//! JSON values by key name and lives in a read-only dashboard crate — it cannot
//! see a credential pasted into a sentence, which is the only shape the learning
//! path ever receives.
//!
//! `context.db`'s own schema documents the consequence: it "holds recalled
//! memories, episodes (verbatim copies of past user prompts), and facts mined
//! from workspace content, so whatever secrets the workspace contains can end up
//! inside it," and narrows the file to `0600` in response. File permissions are
//! a mitigation for a leak, not a substitute for not storing the secret — and
//! they do nothing for the reflection log, which is plain JSONL, or for anything
//! later exported. So this is a real unmet invariant on the reflection path, and
//! this module closes it.
//!
//! ## Why no regex
//!
//! `regex` is not a workspace dependency and adding one for this would be a
//! large surface for a scanner this small. The same reasoning already produced a
//! hand-written FNV in `crate::mining`. Everything here is a single forward pass.
//!
//! ## Posture: fail closed
//!
//! Over-redaction costs a slightly less useful lesson. Under-redaction costs a
//! leaked credential that is then durable, mined, and possibly re-injected into
//! a later prompt. When a heuristic is uncertain, it redacts. The one thing this
//! must not do is silently *look* like it redacted when it did not, so
//! [`Redaction::redacted`] reports whether anything was actually replaced and
//! every caller records that on the record it writes.

/// The result of redacting a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redaction {
    /// The text with every recognized secret replaced by [`PLACEHOLDER`].
    pub text: String,
    /// Whether anything was actually replaced. Recorded on the persisted record
    /// so "this was redacted" is a fact a reader can see, not an assumption.
    pub redacted: bool,
}

/// What a redacted span is replaced with. Fixed rather than length-preserving:
/// a placeholder that encoded the original length would leak the length.
pub const PLACEHOLDER: &str = "[redacted]";

/// Credential prefixes that identify a token on sight. Every one of these is a
/// vendor-assigned, publicly documented prefix, so a match is near-certain
/// rather than heuristic.
const SECRET_PREFIXES: &[&str] = &[
    "sk-",         // OpenAI / Anthropic (sk-ant-…)
    "rk-",         // OpenAI restricted
    "ghp_",        // GitHub personal access token
    "gho_",        // GitHub OAuth
    "ghu_",        // GitHub user-to-server
    "ghs_",        // GitHub server-to-server
    "ghr_",        // GitHub refresh
    "github_pat_", // GitHub fine-grained PAT
    "glpat-",      // GitLab
    "xoxb-",       // Slack bot
    "xoxp-",       // Slack user
    "xoxa-",       // Slack app
    "xoxs-",       // Slack workspace
    "npm_",        // npm automation token
    "dop_v1_",     // DigitalOcean
    "doo_v1_",     // DigitalOcean OAuth
    "sk_live_",    // Stripe secret
    "sk_test_",    // Stripe test secret
    "rk_live_",    // Stripe restricted
    "AKIA",        // AWS access key id
    "ASIA",        // AWS temporary access key id
    "AIza",        // Google API key
    "ya29.",       // Google OAuth
    "SG.",         // SendGrid
    "hf_",         // Hugging Face
    "pat-",        // Airtable / misc
    "shpat_",      // Shopify
    "sq0atp-",     // Square
    "sq0csp-",     // Square
];

/// Key names whose *value* is a credential regardless of how it looks. Matched
/// as a substring of the lowercased key, mirroring the settings scrubber's
/// vocabulary so the two agree about what "sensitive" means.
const SENSITIVE_KEY_MARKERS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "accesskey",
    "access_key",
    "privatekey",
    "private_key",
    "credential",
    "authorization",
    "auth_token",
    "bearer",
    "session_id",
    "client_secret",
];

/// Characters that hold a credential token together. Excludes `=` and `:` so
/// `KEY=value` and `key: value` split into a key, a separator, and a value —
/// which is what lets the key-name rule fire at all.
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | '~')
}

/// Is this run a credential on its own evidence?
fn is_secret_token(token: &str) -> bool {
    if token.len() < 8 {
        return false;
    }
    if SECRET_PREFIXES
        .iter()
        .any(|p| token.starts_with(p) && token.len() > p.len() + 4)
    {
        return true;
    }
    is_jwt(token) || is_high_entropy_blob(token)
}

/// A JWT: three base64url segments, the first of which is a base64-encoded
/// `{"` — which always renders as `eyJ`. Matched by shape rather than by
/// decoding, because a malformed JWT is still a leaked credential.
fn is_jwt(token: &str) -> bool {
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    parts.next().is_none()
        && header.starts_with("eyJ")
        && header.len() >= 8
        && payload.len() >= 8
        && !signature.is_empty()
}

/// A long, mixed-case, digit-bearing opaque blob — the shape of a base64 or
/// hex-plus-case secret with no vendor prefix.
///
/// Deliberately narrow to avoid eating things that are not secrets:
/// * a git SHA is all-lowercase hex, so the mixed-case requirement excludes it;
/// * a file path or URL contains `/` or `.`, so those are excluded outright;
/// * ordinary prose words are far under the length floor.
fn is_high_entropy_blob(token: &str) -> bool {
    if token.len() < 32 || token.contains('/') || token.contains('.') {
        return false;
    }
    let mut upper = false;
    let mut lower = false;
    let mut digit = false;
    for c in token.chars() {
        match c {
            'A'..='Z' => upper = true,
            'a'..='z' => lower = true,
            '0'..='9' => digit = true,
            '_' | '-' | '+' | '~' => {}
            _ => return false,
        }
    }
    upper && lower && digit
}

/// Does this key name mark its value as a credential?
fn is_sensitive_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    // `*_env` names an environment VARIABLE to read a secret from, not the
    // secret — the settings scrubber makes the same exemption.
    if lowered.ends_with("_env") {
        return false;
    }
    SENSITIVE_KEY_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

/// Replace every recognized secret in `text`.
///
/// Four rules, applied in one forward pass:
///
/// 1. a PEM private-key block, header to footer;
/// 2. a token matching a known vendor prefix, a JWT shape, or a long opaque
///    high-entropy blob;
/// 3. the value following a sensitive key name across `=`, `:`, or `":"`;
/// 4. the userinfo of a URL carrying `user:password@host`.
pub fn redact_secrets(text: &str) -> Redaction {
    let bytes: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut redacted = false;
    let mut i = 0usize;

    while i < bytes.len() {
        // 1. PEM blocks — everything between BEGIN and END, inclusive.
        if let Some(end) = pem_block_end(&bytes, i) {
            out.push_str(PLACEHOLDER);
            redacted = true;
            i = end;
            continue;
        }

        if !is_token_char(bytes[i]) {
            out.push(bytes[i]);
            i += 1;
            continue;
        }

        // A maximal run of token characters.
        let start = i;
        while i < bytes.len() && is_token_char(bytes[i]) {
            i += 1;
        }
        let token: String = bytes[start..i].iter().collect();

        // 4. URL userinfo: `scheme://user:pass@host`. The run stops at the `:`,
        //    so the check is "did this run end at a `:` that is followed by
        //    something and then an `@`".
        if let Some(after) = url_userinfo_end(&bytes, i)
            && token.contains("//")
        {
            out.push_str(&token[..token.rfind("//").map_or(0, |p| p + 2)]);
            out.push_str(PLACEHOLDER);
            redacted = true;
            i = after;
            continue;
        }

        // 2. The token speaks for itself.
        if is_secret_token(&token) {
            out.push_str(PLACEHOLDER);
            redacted = true;
            continue;
        }

        out.push_str(&token);

        // 3. A sensitive key name: redact whatever it is assigned.
        if is_sensitive_key(&token)
            && let Some((separator, value_start, value_end)) = assigned_value(&bytes, i)
        {
            out.extend(&bytes[i..separator]);
            out.push(bytes[separator]);
            out.extend(&bytes[separator + 1..value_start]);
            out.push_str(PLACEHOLDER);
            redacted = true;
            i = value_end;
        }
    }

    // `redacted` is what the pass *attempted*; the reported flag is what
    // actually changed. They differ in exactly one case that matters: re-running
    // over already-redacted text re-matches `KEY=[redacted]` on the key-name
    // rule and rewrites the placeholder onto itself. Reporting that as a
    // redaction would make the flag mean "a rule fired" rather than "content was
    // removed", and every replay of an extraction would flip it back on.
    let _ = redacted;
    let changed = out != text;
    Redaction {
        text: out,
        redacted: changed,
    }
}

/// If a PEM private-key header starts at `i`, the index just past its footer
/// (or past the end, for a truncated block — a half-pasted key is still a key).
fn pem_block_end(chars: &[char], i: usize) -> Option<usize> {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    if !starts_with(chars, i, BEGIN) {
        return None;
    }
    // Only private/encrypted key blocks; a CERTIFICATE or PUBLIC KEY is not a
    // secret and redacting it would lose real information.
    let line_end = (i..chars.len())
        .find(|&j| chars[j] == '\n')
        .unwrap_or(chars.len());
    let header: String = chars[i..line_end].iter().collect();
    if !header.contains("PRIVATE KEY") {
        return None;
    }
    let footer = (line_end..chars.len()).find(|&j| starts_with(chars, j, END));
    match footer {
        Some(f) => Some(
            (f..chars.len())
                .find(|&j| chars[j] == '\n')
                .unwrap_or(chars.len()),
        ),
        None => Some(chars.len()),
    }
}

fn starts_with(chars: &[char], at: usize, needle: &str) -> bool {
    needle
        .chars()
        .enumerate()
        .all(|(k, c)| chars.get(at + k) == Some(&c))
}

/// If position `i` is a `:` beginning a URL's `password@` userinfo, the index
/// just past the `@`.
fn url_userinfo_end(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&':') {
        return None;
    }
    let mut j = i + 1;
    while j < chars.len() {
        match chars[j] {
            '@' => return (j > i + 1).then_some(j + 1),
            // Userinfo cannot contain these; anything else is not a credential.
            '/' | ' ' | '\t' | '\n' | '"' | '\'' => return None,
            _ => j += 1,
        }
    }
    None
}

/// If a `=`/`:` assignment follows position `i`, return
/// `(separator_index, value_start, value_end)`.
///
/// Handles the three spellings that appear in prose and pasted config alike:
/// `KEY=value`, `key: value`, and `"key": "value"`. Quotes around the value are
/// preserved so the sentence still reads correctly with the placeholder inside
/// them.
fn assigned_value(chars: &[char], i: usize) -> Option<(usize, usize, usize)> {
    let mut j = i;
    // An optional closing quote from a `"key"` spelling, then whitespace.
    if chars.get(j) == Some(&'"') || chars.get(j) == Some(&'\'') {
        j += 1;
    }
    while matches!(chars.get(j), Some(' ') | Some('\t')) {
        j += 1;
    }
    let separator = j;
    if !matches!(chars.get(separator), Some('=') | Some(':')) {
        return None;
    }
    j = separator + 1;
    while matches!(chars.get(j), Some(' ') | Some('\t')) {
        j += 1;
    }
    let quote = match chars.get(j) {
        Some('"') => Some('"'),
        Some('\'') => Some('\''),
        _ => None,
    };
    if quote.is_some() {
        j += 1;
    }
    let value_start = j;
    let value_end = match quote {
        Some(q) => (value_start..chars.len())
            .find(|&k| chars[k] == q)
            .unwrap_or(chars.len()),
        None => (value_start..chars.len())
            .find(|&k| chars[k].is_whitespace() || matches!(chars[k], ',' | ';' | ')' | '}'))
            .unwrap_or(chars.len()),
    };
    (value_end > value_start).then_some((separator, value_start, value_end))
}

#[cfg(test)]
mod tests;
