//! Credential resolution. Never `Display`/`Debug`-leaks the secret value:
//! [`ApiKey`] has no `Display` at all and a redacted `Debug`, so a credential
//! cannot reach a log line, a trace JSONL record, or a panic message even by
//! accident — `reveal` is the one sanctioned read, for building an auth
//! header.
//!
//! Resolution order: CLI flag -> env var ->
//! provider-native config (`~/.stella/credentials.toml` here; the AWS
//! profile file, AWS SSO/IMDS, and the Google ADC file remain deferred — the
//! Bedrock/Vertex adapters take ready credentials from the chain above, see
//! their module docs) -> interactive prompt on first use, which never
//! silently fails with an opaque provider error.
//!
//! Most providers authenticate with exactly one secret, which is the shape
//! [`ApiKey`] resolves. Bedrock needs a *set* — access key id, secret access
//! key, optional session token, plus a region that is routing rather than a
//! credential. Those extra values travel in [`AuxCredentials`] and are stored
//! in the `[credential_fields.<provider>]` half of the credentials file, so
//! Bedrock has a durable home for all four instead of only working when the
//! standard AWS variables happen to be exported.

// Not `aux`: that spelling made the repository un-checkoutable on Windows.
// `scripts/check-reserved-paths.sh` is what stops it coming back.
mod aux_credentials;
mod prompt;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use thiserror::Error;
use zeroize::Zeroize;

pub use aux_credentials::AuxCredentials;
pub use prompt::{CredentialPrompt, TerminalPrompt};

/// `Clone` because every variant is owned strings with no secret in them — a
/// caller that fans one failure out to several reports (or a test that scripts
/// the same error into repeated prompts) should not have to rebuild it.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    #[error(
        "no credential found for `{env_var}` — set the environment variable, add it to \
         ~/.stella/credentials.toml, or run interactively to be prompted"
    )]
    NotFound { env_var: String },
    #[error("credential for `{env_var}` is empty")]
    Empty { env_var: String },
    #[error("failed to read credentials file {path}: {message}")]
    FileRead { path: String, message: String },
    #[error("failed to parse credentials file {path}: {message}")]
    FileParse { path: String, message: String },
    #[error("failed to write credentials file {path}: {message}")]
    FileWrite { path: String, message: String },
    #[error("interactive prompt failed: {0}")]
    PromptFailed(String),
    #[error("Vertex AI needs a project id — set VERTEX_PROJECT_ID (or GOOGLE_CLOUD_PROJECT)")]
    VertexProjectMissing,
    #[error("Bedrock needs AWS_SECRET_ACCESS_KEY alongside AWS_ACCESS_KEY_ID")]
    BedrockSecretMissing,
}

/// Which step in the resolution chain produced an [`ApiKey`]. Exists so
/// callers (and tests) can assert precedence without inspecting the secret
/// itself, and so a successful interactive prompt can be *identified* for
/// write-back — only `Interactive` results are worth persisting, since every
/// other source already has a durable home.
///
/// [`ApiKey::resolve`] never writes anything itself: it borrows the
/// credentials file (`&CredentialsFile`), so it structurally cannot. The
/// write-back is the caller's, keyed off this discriminant —
/// `stella-cli`'s `Config::resolve` sets the row and saves, and treats a
/// failed save as a warning rather than failing the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    /// An explicit `--api-key`-style flag on the invocation — highest
    /// precedence, since it is the most deliberate thing the user can say.
    CliFlag,
    /// The provider's documented environment variable (`ANTHROPIC_API_KEY`,
    /// `ZAI_API_KEY`, …).
    EnvVar,
    /// A `[credentials]` row in `~/.stella/credentials.toml` — including one
    /// a previous interactive prompt wrote back.
    ConfigFile,
    /// A literal `providers.<id>.api_key` in a merged settings scope
    /// (user/org/project `settings.json`). `ApiKey::resolve` itself never
    /// produces this — it has no notion of settings.json — a caller that
    /// layers a settings literal on top of the base chain (`stella-cli`'s
    /// `resolve_provider_key`) constructs it, so a display surface can tell
    /// "this file" (`ConfigFile`) apart from "this declarative config"
    /// (`SettingsJson`) instead of both reporting as the same source.
    SettingsJson,
    /// Typed at the masked prompt on first use. The one source worth
    /// persisting — see the write-back note on [`CredentialSource`] itself
    /// for why the write is the caller's, not `resolve`'s.
    Interactive,
}

/// A secret API key. Deliberately has no `Display` and a redacted `Debug`
/// so a stray `println!`/`tracing::info!` can never leak it, and its
/// plaintext is **wiped on drop** ([`Zeroize`]) rather than left legible in
/// freed heap for the lifetime of the process.
///
/// What zeroizing does and does not buy, stated plainly so nobody reads more
/// into it than is true: the buffer this `ApiKey` owns *at drop time* is
/// overwritten through a volatile write the optimiser may not elide. It
/// cannot reach a copy someone else already made — a `String` that grew and
/// reallocated, a page the OS swapped out, or a `HeaderValue` reqwest built
/// from `reveal()`. Every copy this crate makes on purpose is wrapped in
/// [`Zeroizing`](zeroize::Zeroizing) at its call site; the ones inside a third-party HTTP stack
/// are out of our reach and are not claimed to be covered.
#[derive(Clone)]
pub struct ApiKey(String);

impl Drop for ApiKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl ApiKey {
    /// Wrap an already-resolved secret. The narrow constructor for callers
    /// that obtained the value some other way (a settings literal, a test
    /// fixture, an AWS/Vertex token minted elsewhere); the resolution chain
    /// itself goes through [`ApiKey::resolve`].
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Resolve from an environment variable only. Kept as the narrow,
    /// single-step primitive the full chain (`resolve`) and simpler
    /// call sites build on.
    pub fn from_env(env_var: &str) -> Result<Self, CredentialError> {
        match std::env::var(env_var) {
            Ok(value) if !value.is_empty() => Ok(Self(value)),
            Ok(_) => Err(CredentialError::Empty {
                env_var: env_var.to_string(),
            }),
            Err(_) => Err(CredentialError::NotFound {
                env_var: env_var.to_string(),
            }),
        }
    }

    /// The full resolution chain: CLI flag -> env
    /// var -> `credentials_file` -> interactive prompt. `provider_id` keys
    /// both the credentials-file lookup and, on a successful interactive
    /// prompt, what gets written back so the user is only ever prompted
    /// once. `interactive` gates the last step — headless/non-interactive
    /// callers (CI, `--output-format stream-json`) should pass `false` and
    /// get a clean [`CredentialError::NotFound`] instead of hanging on a
    /// read from a stdin that isn't there.
    ///
    /// The prompt is [`TerminalPrompt`]; [`ApiKey::resolve_with_prompt`] is
    /// the same chain over any other [`CredentialPrompt`].
    pub fn resolve(
        provider_id: &str,
        env_var: &str,
        cli_flag: Option<&str>,
        credentials_file: Option<&CredentialsFile>,
        interactive: bool,
    ) -> Result<(Self, CredentialSource), CredentialError> {
        Self::resolve_with_prompt(
            provider_id,
            env_var,
            cli_flag,
            credentials_file,
            interactive,
            &TerminalPrompt,
        )
    }

    /// [`ApiKey::resolve`] with the interactive step supplied rather than
    /// assumed.
    ///
    /// A host that already knows how it talks to its user — a GUI, a daemon
    /// with its own secret store, a test — injects that here instead of
    /// inheriting `rpassword` on the controlling terminal. It is also the only
    /// way to reach the swallow below deterministically: the shipping
    /// [`TerminalPrompt`] declines under `cargo test`, where stdout is not a
    /// terminal, so a test driving `resolve` alone exercises the gate and
    /// never the arm underneath it (#4576).
    pub fn resolve_with_prompt(
        provider_id: &str,
        env_var: &str,
        cli_flag: Option<&str>,
        credentials_file: Option<&CredentialsFile>,
        interactive: bool,
        prompt: &dyn CredentialPrompt,
    ) -> Result<(Self, CredentialSource), CredentialError> {
        if let Some(flag_value) = cli_flag
            && !flag_value.is_empty()
        {
            return Ok((Self(flag_value.to_string()), CredentialSource::CliFlag));
        }

        match Self::from_env(env_var) {
            Ok(key) => return Ok((key, CredentialSource::EnvVar)),
            Err(CredentialError::Empty { env_var }) => {
                return Err(CredentialError::Empty { env_var });
            }
            Err(CredentialError::NotFound { .. }) => {} // fall through
            Err(other) => return Err(other),
        }

        if let Some(file) = credentials_file
            && let Some(value) = file.get(provider_id)
        {
            return Ok((Self(value.to_string()), CredentialSource::ConfigFile));
        }

        if prompt.can_prompt(interactive) {
            // A successful prompt returns immediately. A failure — most
            // commonly because stdin reports as a terminal (`is_terminal()`
            // == true) yet is not genuinely readable (the libtest pty,
            // headless `cargo run` through a pipe, or a closed stdin all
            // trigger an immediate IO error like `ENXIO`, "Device not
            // configured" from the underlying password reader) — is NOT
            // propagated. Per the documented contract, an unusable stdin
            // degrades to a clean [`CredentialError::NotFound`] rather than
            // surfacing the opaque [`PromptFailed`] at this trust boundary —
            // never hang, never leak an opaque error.
            if let Ok(value) = prompt.ask(provider_id, env_var) {
                return Ok((Self(value), CredentialSource::Interactive));
            }
        }

        Err(CredentialError::NotFound {
            env_var: env_var.to_string(),
        })
    }

    /// The only sanctioned way to read the secret value — for building an
    /// auth header, nothing else.
    pub fn reveal(&self) -> &str {
        &self.0
    }

    /// A non-reversible preview for human display (e.g. `stella config`):
    /// a few leading and trailing characters with the middle elided, so a
    /// user can eyeball *which* key is active without the full secret hitting
    /// the terminal. Char-boundary-safe (never panics on multi-byte input)
    /// and never panics on short keys — a key too short to partially reveal
    /// without effectively exposing it is shown fully masked instead. This is
    /// the safe replacement for the old `&self.api_key[..8]` byte-slice, which
    /// panicked both on keys shorter than 8 bytes and on non-ASCII boundaries.
    pub fn redacted_preview(&self) -> String {
        const HEAD: usize = 6;
        const TAIL: usize = 4;
        let chars: Vec<char> = self.0.chars().collect();
        // Require enough length that head+tail don't overlap and the elided
        // middle actually hides something; otherwise mask entirely.
        if chars.len() <= HEAD + TAIL + 2 {
            return "•".repeat(chars.len().min(8));
        }
        let head: String = chars[..HEAD].iter().collect();
        let tail: String = chars[chars.len() - TAIL..].iter().collect();
        format!("{head}…{tail}")
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ApiKey").field(&"<redacted>").finish()
    }
}

/// `~/.stella/credentials.toml` — optional provider keys for users
/// who prefer file storage over env vars. Written
/// with `0600` permissions on Unix (owner read/write only) since it holds
/// secrets in plaintext, same threat model as `~/.ssh/config`.
///
/// Shape: `[credentials]` table, `provider_id = "key"` per row — flat and
/// small on purpose; this is a handful of BYOK keys, not a config language.
///
/// Deliberately NOT `Debug`: the map holds plaintext provider keys, so a
/// derived `Debug` would print every secret in the file. [`CredentialsFile`]
/// formats itself with a hand-written redacting impl instead — the same
/// posture as [`ApiKey`] above and every other secret-bearing type in the
/// workspace.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct CredentialsFileData {
    #[serde(default)]
    credentials: BTreeMap<String, String>,
    /// `[credential_fields.<provider_id>]` — the values a provider needs
    /// *beyond* its one key, keyed by the same canonical environment-variable
    /// name they carry everywhere else (`AWS_SECRET_ACCESS_KEY`,
    /// `AWS_REGION`). Bedrock is the only provider that has any today.
    ///
    /// A second table rather than dotted rows inside `[credentials]`: that
    /// table deserializes as `provider_id -> key`, so a dotted key there
    /// would parse as a nested table and fail the whole file. Keeping the
    /// nesting explicit also keeps the one-key common case exactly as flat as
    /// it was — a user with no Bedrock credentials never sees this section.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    credential_fields: BTreeMap<String, BTreeMap<String, String>>,
}

/// Every stored key is plaintext and the map outlives the calls that read it,
/// so the whole table is wiped when the file object goes away — the same
/// posture [`ApiKey`] takes for a single key. The auxiliary fields hold
/// secrets too (a Bedrock secret access key is exactly as sensitive as any
/// API key) and are wiped with them.
impl Drop for CredentialsFileData {
    fn drop(&mut self) {
        for value in self.credentials.values_mut() {
            value.zeroize();
        }
        for fields in self.credential_fields.values_mut() {
            for value in fields.values_mut() {
                value.zeroize();
            }
        }
    }
}

/// Something worth telling the user about a credentials file we just read —
/// never a reason to refuse the read.
///
/// The distinction is the whole point of this type. `save` creates the file
/// `0600` from birth, so a loose mode means the file came from somewhere else
/// (hand-written, restored from a backup, checked out from a dotfiles repo).
/// Two responses were rejected: **refusing** would lock a user out of their
/// own credentials over a condition they may not be able to fix from inside
/// stella, and **silently `chmod`-ing** would change the mode of a file we did
/// not create, which is not ours to do. So we warn, once, and read it anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialAdvisory {
    /// The credentials file is readable, writable, or executable by group or
    /// other. `mode` is the permission bits as found (`& 0o777`).
    LoosePermissions { path: String, mode: u32 },
}

impl CredentialAdvisory {
    /// The one-line form a launch path prints (and tests pin).
    pub fn line(&self) -> String {
        match self {
            CredentialAdvisory::LoosePermissions { path, mode } => format!(
                "{path} is mode {mode:04o} — its plaintext provider keys are readable beyond \
                 your account. stella did not create it this way; fix it with `chmod 600 \
                 {path}` (the keys were still read for this run)."
            ),
        }
    }
}

pub struct CredentialsFile {
    path: PathBuf,
    data: CredentialsFileData,
    advisories: Vec<CredentialAdvisory>,
}

/// Names the file and how many keys it holds; never a key, and never a
/// provider id's value. Redaction is the whole point — `{:?}` on a loaded
/// credentials file used to dump every plaintext secret in it.
impl fmt::Debug for CredentialsFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialsFile")
            .field("path", &self.path)
            .field("providers", &self.provider_ids().count())
            .field("credentials", &"<redacted>")
            .finish()
    }
}

impl CredentialsFile {
    /// The default path: `$STELLA_HOME/credentials.toml`, else
    /// `~/.stella/credentials.toml`. Returns `None` if the platform has no
    /// resolvable home directory (never panics — callers treat "no credentials
    /// file available" as just another resolution step that falls through).
    ///
    /// Resolution goes through `stella-home`, the workspace's single
    /// implementation, for two reasons that used to be defects here. It
    /// honours `STELLA_HOME`, so an isolated process reads the scratch home's
    /// keys rather than the developer's real ones — this file was previously
    /// unmovable by any environment variable at all (#2178). And it falls back
    /// to `USERPROFILE`, so Windows, where `HOME` is usually unset, gets the
    /// file step of the chain instead of silently dropping to env vars and
    /// `--api-key`.
    pub fn default_path() -> Option<PathBuf> {
        Some(stella_home::stella_home()?.join("credentials.toml"))
    }

    /// Load from `path`. A missing file is not an error — it's the common
    /// case for anyone using env vars — and yields an empty file ready to
    /// be populated by a later interactive prompt + `save`.
    ///
    /// A file whose mode lets group or other read it is loaded **anyway**,
    /// with a [`CredentialAdvisory`] recorded on the returned value (see
    /// [`CredentialsFile::advisories`]). The advisory describes the file as
    /// found at load time and is deliberately not cleared by a later `save`:
    /// the point it makes — "your secrets were read out of a world-readable
    /// file on this run" — stays true after the mode is tightened.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, CredentialError> {
        let path = path.into();
        let mut advisories = Vec::new();
        let data = match std::fs::read_to_string(&path) {
            Ok(contents) => {
                advisories = permission_advisories(&path);
                toml::from_str(&contents).map_err(|e| CredentialError::FileParse {
                    path: path.display().to_string(),
                    message: e.to_string(),
                })?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => CredentialsFileData::default(),
            Err(e) => {
                return Err(CredentialError::FileRead {
                    path: path.display().to_string(),
                    message: e.to_string(),
                });
            }
        };
        Ok(Self {
            path,
            data,
            advisories,
        })
    }

    /// Advisories raised while reading this file — a loose file mode, today.
    /// Empty is the normal case. A host is expected to surface these once at
    /// startup; an advisory nothing prints is worse than no check at all,
    /// because it reads as a guarantee the code does not actually deliver.
    pub fn advisories(&self) -> &[CredentialAdvisory] {
        &self.advisories
    }

    /// Load from the default path (`default_path`), or an empty in-memory
    /// file when no home directory is resolvable — never fails outright,
    /// since a credentials file is always optional.
    pub fn load_default() -> Result<Self, CredentialError> {
        match Self::default_path() {
            Some(path) => Self::load(path),
            None => Ok(Self::empty()),
        }
    }

    /// An empty, in-memory-only credentials file (no backing path — `save`
    /// on it errors with `FileWrite`). For callers that must always have
    /// *some* `&CredentialsFile` to pass into `ApiKey::resolve` /
    /// `resolve_provider_key`, even when the real file failed to load (e.g.
    /// malformed TOML) and the caller's posture is "degrade the listing,
    /// don't crash it" rather than propagate the error.
    pub fn empty() -> Self {
        Self {
            path: PathBuf::new(),
            data: CredentialsFileData::default(),
            advisories: Vec::new(),
        }
    }

    /// The key stored for `provider_id`, if any.
    pub fn get(&self, provider_id: &str) -> Option<&str> {
        self.data.credentials.get(provider_id).map(String::as_str)
    }

    /// Every provider id currently stored, alphabetically (the underlying
    /// map is a `BTreeMap`, so this is already sorted) — for `stella auth
    /// list`, which enumerates the file rather than looking up one id.
    pub fn provider_ids(&self) -> impl Iterator<Item = &str> {
        self.data.credentials.keys().map(String::as_str)
    }

    /// Set (or replace) `provider_id`'s key in memory. Call `save` to
    /// persist — kept separate so a caller can batch multiple sets into one
    /// write.
    pub fn set(&mut self, provider_id: &str, value: impl Into<String>) {
        self.data
            .credentials
            .insert(provider_id.to_string(), value.into());
    }

    /// Remove `provider_id`'s key **and every auxiliary field** from memory,
    /// if present. Returns whether anything existed. Call `save` to persist.
    ///
    /// Both halves go together on purpose: a Bedrock row whose access key id
    /// was removed but whose secret access key stayed behind is a live secret
    /// in a file the user believes they emptied.
    pub fn remove(&mut self, provider_id: &str) -> bool {
        let had_key = self.data.credentials.remove(provider_id).is_some();
        let had_fields =
            self.data
                .credential_fields
                .remove(provider_id)
                .is_some_and(|mut fields| {
                    let present = !fields.is_empty();
                    for value in fields.values_mut() {
                        value.zeroize();
                    }
                    present
                });
        had_key || had_fields
    }

    /// One auxiliary field for `provider_id`, keyed by its canonical
    /// environment-variable name (`AWS_SECRET_ACCESS_KEY`, `AWS_REGION`).
    pub fn field(&self, provider_id: &str, name: &str) -> Option<&str> {
        self.data
            .credential_fields
            .get(provider_id)
            .and_then(|fields| fields.get(name))
            .map(String::as_str)
    }

    /// Set (or replace) one auxiliary field. Call `save` to persist.
    pub fn set_field(&mut self, provider_id: &str, name: &str, value: impl Into<String>) {
        self.data
            .credential_fields
            .entry(provider_id.to_string())
            .or_default()
            .insert(name.to_string(), value.into());
    }

    /// Every auxiliary field name stored for `provider_id`, alphabetically.
    /// Names only — a display surface may print these; the values are secrets.
    pub fn field_names(&self, provider_id: &str) -> impl Iterator<Item = &str> {
        self.data
            .credential_fields
            .get(provider_id)
            .into_iter()
            .flat_map(|fields| fields.keys().map(String::as_str))
    }

    /// Write the current in-memory state to disk, creating parent
    /// directories as needed. The write is **atomic** and the secret file is
    /// created with owner-only (`0600`) permissions **from birth** on Unix:
    /// contents go to a sibling temp file opened `0600`, which is then
    /// `rename`d over the target (an atomic filesystem operation on the same
    /// directory). This closes the prior TOCTOU window where the file existed
    /// world-readable (`0644`) between `write` and the follow-up `chmod`, and
    /// guarantees no reader ever sees a half-written credentials file.
    /// (Best-effort on non-Unix — Windows ACLs are a different mechanism,
    /// out of scope here.)
    ///
    /// The *directory* is not hardened the same way: `create_dir_all` takes
    /// the process umask, so `~/.stella` is typically `0755` — world-listable,
    /// though the secret file inside it stays `0600`, so no other user can
    /// read a key. Two follow-ups worth taking together if this file's threat
    /// model ever includes a hostile local user with write access to the
    /// directory: create it `0700`, and open the temp file `create_new` so a
    /// pre-planted symlink at the predictable `.tmp.<pid>` name cannot
    /// redirect the write.
    pub fn save(&self) -> Result<(), CredentialError> {
        if self.path.as_os_str().is_empty() {
            return Err(CredentialError::FileWrite {
                path: "<none>".into(),
                message: "no resolvable home directory — cannot persist credentials".into(),
            });
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CredentialError::FileWrite {
                path: self.path.display().to_string(),
                message: e.to_string(),
            })?;
        }
        let contents =
            toml::to_string_pretty(&self.data).map_err(|e| CredentialError::FileWrite {
                path: self.path.display().to_string(),
                message: e.to_string(),
            })?;

        // Temp file in the SAME directory so the final `rename` is atomic
        // (a cross-filesystem rename would fall back to copy+delete and lose
        // atomicity). The pid suffix keeps two *processes* — a `stella auth`
        // invocation and a running deck — off each other's temp file; it does
        // NOT separate two concurrent `save` calls inside one process, which
        // would share the name. Every caller today saves from a single
        // command path, so that case is unreached; a second in-process writer
        // needs a per-save counter here (the shape `zai.rs`'s `SESSION_SEQ`
        // uses), not just the pid.
        let tmp_path = self
            .path
            .with_extension(format!("tmp.{}", std::process::id()));
        write_secret_file(&tmp_path, contents.as_bytes()).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp_path);
        })?;
        std::fs::rename(&tmp_path, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            CredentialError::FileWrite {
                path: self.path.display().to_string(),
                message: e.to_string(),
            }
        })?;
        Ok(())
    }
}

/// Inspect a credentials file we did **not** create and report — never
/// enforce — a mode that lets anyone but the owner at it.
///
/// `0o077` covers group/other read, write, and execute: a group-*writable*
/// credentials file is strictly worse than a group-readable one (someone else
/// can substitute a key), so both are flagged by the same advisory rather than
/// splitting hairs the user cannot act on differently — `chmod 600` is the fix
/// for every bit in the mask. A file we cannot `stat` yields no advisory: it
/// was readable a moment ago, and inventing a warning from a failed metadata
/// call would be noise, not information.
#[cfg(unix)]
fn permission_advisories(path: &Path) -> Vec<CredentialAdvisory> {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = std::fs::metadata(path) else {
        return Vec::new();
    };
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        return Vec::new();
    }
    vec![CredentialAdvisory::LoosePermissions {
        path: path.display().to_string(),
        mode,
    }]
}

/// Windows expresses this through ACLs, which are not a mode word; there is
/// no honest check to make here, so no advisory is invented. (Claiming one
/// would be the false-assurance failure this whole channel exists to avoid.)
#[cfg(not(unix))]
fn permission_advisories(_path: &Path) -> Vec<CredentialAdvisory> {
    Vec::new()
}

/// One IO failure on the secret-file write path as a named
/// [`CredentialError::FileWrite`]. Shared by every step of
/// [`write_secret_file`] so open, chmod, write, and fsync failures all name
/// the same path rather than each spelling the conversion itself.
fn write_err(path: &Path, e: std::io::Error) -> CredentialError {
    CredentialError::FileWrite {
        path: path.display().to_string(),
        message: e.to_string(),
    }
}

/// Write `bytes` to `path`, creating the file with `0600` permissions from
/// the moment it exists on Unix (via `OpenOptions::mode`, applied at
/// creation — never a create-then-chmod race). `0600` has no group/other
/// bits, so it is unaffected by any reasonable `umask`.
///
/// The contents are `fsync`ed before returning, so the caller's follow-up
/// `rename` cannot publish a name whose data is still only in the page
/// cache: a crash between the two would otherwise leave a credentials file
/// that exists but reads back empty or truncated, which is worse than the
/// old file surviving. (`Write::flush` on a `File` is a no-op — it has no
/// userspace buffer — so it never provided this.) The parent directory
/// entry itself is still not fsynced; a crash immediately after `rename`
/// can therefore lose the *replacement* on a hard-crash filesystem, which
/// is the acceptable half of the tradeoff — the previous, valid file
/// survives.
#[cfg(unix)]
fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), CredentialError> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| write_err(path, e))?;
    // Belt-and-suspenders: if the temp path already existed (e.g. a crashed
    // prior run), `mode` on `open` does not reset an existing file's perms,
    // so force them here before writing any secret bytes.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| write_err(path, e))?;
    file.write_all(bytes).map_err(|e| write_err(path, e))?;
    file.sync_all().map_err(|e| write_err(path, e))?;
    Ok(())
}

/// The non-Unix fallback, and honest about what it does not do: no birth-mode
/// permissions (Windows ACLs are a different mechanism, out of scope) and no
/// `fsync`, so the caller's `rename` can publish a name whose bytes are still
/// only in the page cache. The atomicity claim on [`CredentialsFile::save`] is
/// therefore a Unix claim. Unreached today — [`CredentialsFile::default_path`]
/// resolves nothing off `HOME`-less platforms — so this is a placeholder for
/// the port, not a shipped path.
#[cfg(not(unix))]
fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), CredentialError> {
    std::fs::write(path, bytes).map_err(|e| write_err(path, e))
}

/// Read `name` from the process environment, treating an explicitly-empty
/// value as absent (the same posture `resolve`'s env step takes for keys).
fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Vertex AI's non-secret addressing: which GCP project and location the
/// request is scoped to. Distinct from [`ApiKey`] — the bearer token still
/// comes through [`ApiKey::resolve`] as `VERTEX_ACCESS_TOKEN`; this is the
/// project/location pair `VertexProvider::new` needs alongside it.
///
/// Lives here rather than in the CLI so a second host of the engine can
/// construct a `VertexProvider` without copying the variable names and the
/// `global` default (see `vertex.rs`'s addressing note).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexAddressing {
    /// GCP project id — `VERTEX_PROJECT_ID`, else `GOOGLE_CLOUD_PROJECT`.
    pub project: String,
    /// Vertex location — `VERTEX_LOCATION`, defaulting to `global`.
    pub location: String,
}

impl VertexAddressing {
    /// Resolve from the process environment.
    pub fn resolve() -> Result<Self, CredentialError> {
        Self::resolve_from(env_non_empty)
    }

    /// The resolution itself, over an injected lookup — pure, so the
    /// fallback order and the `global` default are unit-testable without
    /// mutating the process environment (which is `unsafe` under edition
    /// 2024 and races across parallel test threads).
    pub(crate) fn resolve_from(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, CredentialError> {
        let project = lookup("VERTEX_PROJECT_ID")
            .or_else(|| lookup("GOOGLE_CLOUD_PROJECT"))
            .ok_or(CredentialError::VertexProjectMissing)?;
        let location = lookup("VERTEX_LOCATION").unwrap_or_else(|| "global".to_string());
        Ok(Self { project, location })
    }
}

/// Amazon Bedrock's secondary credentials and region. The access key id
/// arrives through [`ApiKey::resolve`] as `AWS_ACCESS_KEY_ID`; SigV4 also
/// needs the secret, optionally a session token, and the region the endpoint
/// host is built from.
///
/// Lives here rather than in the CLI so a second host of the engine can
/// construct a `BedrockProvider` without copying the variable names, the
/// `AWS_REGION` → `AWS_DEFAULT_REGION` order, or the `us-east-1` default.
#[derive(Debug, Clone)]
pub struct BedrockCredentials {
    /// `AWS_SECRET_ACCESS_KEY` — required; SigV4 cannot sign without it.
    pub secret_access_key: ApiKey,
    /// `AWS_SESSION_TOKEN` — present only for temporary credentials.
    pub session_token: Option<ApiKey>,
    /// `AWS_REGION`, else `AWS_DEFAULT_REGION`, else `us-east-1`.
    pub region: String,
}

impl BedrockCredentials {
    /// Every environment-variable name this resolves *beyond* the access key
    /// id, in the order a host should offer them. Public because the host is
    /// what walks its own chain (an inherited descriptor, the environment,
    /// `~/.stella/credentials.toml`) for each name and hands the results back
    /// as an [`AuxCredentials`] — it must not have to re-spell this list, or
    /// the two copies drift and a value silently stops resolving.
    pub const AUX_ENV_NAMES: &'static [&'static str] = &[
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
    ];

    /// The subset of [`Self::AUX_ENV_NAMES`] that is genuinely a secret, for
    /// hosts that route secrets and routing values through different seams
    /// (the benchmark launcher sends these over an anonymous descriptor and
    /// the region as an ordinary variable). The complement — `AWS_REGION` and
    /// `AWS_DEFAULT_REGION` — is addressing: required, but not a credential.
    pub const SECRET_ENV_NAMES: &'static [&'static str] =
        &["AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"];

    /// Resolve from the process environment.
    pub fn resolve() -> Result<Self, CredentialError> {
        Self::resolve_from(env_non_empty)
    }

    /// Resolve from values a host already resolved through its own chain,
    /// falling back to the process environment for anything absent.
    ///
    /// The fallback is what keeps a library-style caller — one that builds a
    /// provider straight off [`crate::factory::build_provider`] with no chain
    /// of its own — working exactly as it did when this read the environment
    /// directly. A host that *has* a chain resolves every name in
    /// [`Self::AUX_ENV_NAMES`] itself and wins here, which is what lets a
    /// sealed process (benchmark claim mode, where the AWS variables are
    /// deliberately absent from the environment) reach Bedrock at all.
    pub fn resolve_with(aux: &AuxCredentials) -> Result<Self, CredentialError> {
        Self::resolve_from(|name| {
            aux.get(name)
                .map(str::to_string)
                .or_else(|| env_non_empty(name))
        })
    }

    /// The resolution itself, over an injected lookup — see
    /// [`VertexAddressing::resolve_from`] for why.
    pub(crate) fn resolve_from(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, CredentialError> {
        let secret_access_key = lookup("AWS_SECRET_ACCESS_KEY")
            .map(ApiKey::new)
            .ok_or(CredentialError::BedrockSecretMissing)?;
        let session_token = lookup("AWS_SESSION_TOKEN").map(ApiKey::new);
        let region = lookup("AWS_REGION")
            .or_else(|| lookup("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|| "us-east-1".to_string());
        Ok(Self {
            secret_access_key,
            session_token,
            region,
        })
    }
}

#[cfg(test)]
mod secondary_credential_tests;
#[cfg(test)]
mod tests;
