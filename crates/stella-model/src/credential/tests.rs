use super::*;

fn temp_credentials_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "stella-test-credentials-{name}-{}.toml",
        std::process::id()
    ))
}

#[test]
fn debug_never_prints_the_secret_value() {
    let key = ApiKey::new("sk-super-secret-value");
    let debug = format!("{key:?}");
    assert!(!debug.contains("sk-super-secret-value"));
    assert!(debug.contains("redacted"));
}

/// The #3036 witness: a redirected stdout must decline the prompt even
/// with a live keyboard on stdin — the disagreeing configuration a
/// stdin-only check could not see. On `main` before this fix,
/// `ApiKey::resolve`'s interactive gate was `interactive &&
/// stdin_is_terminal` alone, so a `stella config > out.txt` from a real
/// terminal would have `rpassword` write its "enter it now" prompt where
/// nobody could read it and then block on an answer nobody knew to give.
#[test]
fn a_redirected_stdout_declines_the_prompt_even_with_a_live_stdin() {
    assert!(
        !ApiKey::can_prompt_interactively(true, true, false),
        "a human at the keyboard is not enough if the prompt itself is invisible"
    );
}

/// The happy path, named explicitly so the witness above reads as a
/// missing condition rather than an always-false predicate.
#[test]
fn a_full_terminal_can_host_the_prompt() {
    assert!(ApiKey::can_prompt_interactively(true, true, true));
}

#[test]
fn auxiliary_fields_survive_a_save_and_reload() {
    // Bedrock's whole "in general" gap: the secret access key and region
    // had nowhere durable to live, so a stored access key id was a row
    // that looked configured and could not sign.
    let path = temp_credentials_path("aux-roundtrip");
    let _ = std::fs::remove_file(&path);
    let mut file = CredentialsFile::load(&path).unwrap();
    file.set("bedrock", "AKIAEXAMPLEACCESSKEY");
    file.set_field("bedrock", "AWS_SECRET_ACCESS_KEY", "the-secret-access-key");
    file.set_field("bedrock", "AWS_REGION", "ap-southeast-2");
    file.save().unwrap();

    let reloaded = CredentialsFile::load(&path).unwrap();
    assert_eq!(reloaded.get("bedrock"), Some("AKIAEXAMPLEACCESSKEY"));
    assert_eq!(
        reloaded.field("bedrock", "AWS_SECRET_ACCESS_KEY"),
        Some("the-secret-access-key")
    );
    assert_eq!(
        reloaded.field("bedrock", "AWS_REGION"),
        Some("ap-southeast-2")
    );
    assert_eq!(reloaded.field("bedrock", "AWS_SESSION_TOKEN"), None);
    assert_eq!(
        reloaded.field_names("bedrock").collect::<Vec<_>>(),
        vec!["AWS_REGION", "AWS_SECRET_ACCESS_KEY"]
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_file_with_no_auxiliary_fields_stays_exactly_as_flat_as_before() {
    // The one-key common case must not grow a section it never uses —
    // both because it is noise and because a `[credential_fields]` header
    // in an otherwise-empty file reads as configuration that is missing.
    let path = temp_credentials_path("aux-absent");
    let _ = std::fs::remove_file(&path);
    let mut file = CredentialsFile::load(&path).unwrap();
    file.set("zai", "sk-value");
    file.save().unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("credential_fields"), "got: {written}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn removing_a_provider_takes_its_auxiliary_secrets_with_it() {
    // A "removed" Bedrock row that left AWS_SECRET_ACCESS_KEY behind is a
    // live secret in a file the user believes they emptied.
    let mut file = CredentialsFile::load(temp_credentials_path("aux-remove")).unwrap();
    file.set("bedrock", "AKIAEXAMPLEACCESSKEY");
    file.set_field("bedrock", "AWS_SECRET_ACCESS_KEY", "the-secret-access-key");

    assert!(file.remove("bedrock"));
    assert_eq!(file.get("bedrock"), None);
    assert_eq!(file.field("bedrock", "AWS_SECRET_ACCESS_KEY"), None);
    assert_eq!(file.field_names("bedrock").count(), 0);
}

#[test]
fn removing_reports_true_for_a_provider_that_only_had_fields() {
    let mut file = CredentialsFile::load(temp_credentials_path("aux-only")).unwrap();
    file.set_field("bedrock", "AWS_REGION", "us-west-2");
    assert!(file.remove("bedrock"));
    assert!(!file.remove("bedrock"));
}

#[test]
fn bedrock_prefers_host_resolved_values_over_the_environment() {
    // `resolve_with` is what lets a sealed process reach Bedrock at all:
    // the AWS variables are deliberately absent there, so the aux set has
    // to be able to answer on its own.
    let mut aux = AuxCredentials::new();
    aux.insert("AWS_SECRET_ACCESS_KEY", "secret-from-the-host-chain");
    aux.insert("AWS_SESSION_TOKEN", "session-from-the-host-chain");
    aux.insert("AWS_REGION", "sa-east-1");

    let resolved = BedrockCredentials::resolve_with(&aux).unwrap();
    assert_eq!(
        resolved.secret_access_key.reveal(),
        "secret-from-the-host-chain"
    );
    assert_eq!(
        resolved.session_token.as_ref().map(ApiKey::reveal),
        Some("session-from-the-host-chain")
    );
    assert_eq!(resolved.region, "sa-east-1");
}

#[test]
fn every_bedrock_secret_name_is_also_an_aux_name() {
    // Two lists, one truth: a host routes `SECRET_ENV_NAMES` through its
    // secret seam and the rest as ordinary values, so a name in the first
    // list but not the second would be sent and never read.
    for name in BedrockCredentials::SECRET_ENV_NAMES {
        assert!(
            BedrockCredentials::AUX_ENV_NAMES.contains(name),
            "{name} is declared a secret but is never resolved"
        );
    }
    assert!(!BedrockCredentials::SECRET_ENV_NAMES.contains(&"AWS_REGION"));
}

#[test]
fn redacted_preview_masks_short_keys_without_panicking_or_revealing() {
    // The old `&key[..8]` panicked on keys shorter than 8 bytes; the
    // preview must never panic and must not echo a short secret verbatim.
    for short in ["", "a", "short", "sk-1234"] {
        let preview = ApiKey::new(short).redacted_preview();
        assert!(
            !preview.contains(short) || short.is_empty(),
            "short key `{short}` leaked in preview `{preview}`"
        );
        assert!(
            !preview
                .chars()
                .any(|c| c.is_ascii_alphanumeric() && short.contains(c))
        );
    }
}

#[test]
fn redacted_preview_handles_non_ascii_keys_without_panicking() {
    // The old byte-slice `&key[..8]` panicked on a non-char boundary;
    // a multi-byte key must be previewed safely.
    let preview = ApiKey::new("kéy-with-mültibyte-çharacters-1234").redacted_preview();
    assert!(preview.contains('…'));
    assert!(!preview.contains("mültibyte"));
}

#[test]
fn redacted_preview_shows_head_and_tail_but_elides_the_middle() {
    let preview = ApiKey::new("sk-abcdefghijklmnopqrstuvwxyz-9876").redacted_preview();
    assert!(preview.starts_with("sk-abc"), "{preview}");
    assert!(preview.ends_with("9876"), "{preview}");
    assert!(preview.contains('…'));
    assert!(
        !preview.contains("ghijkl"),
        "middle must be elided: {preview}"
    );
}

#[test]
fn from_env_missing_var_is_not_found() {
    let err = ApiKey::from_env("STELLA_TEST_DEFINITELY_UNSET_VAR_12345").unwrap_err();
    assert_eq!(
        err,
        CredentialError::NotFound {
            env_var: "STELLA_TEST_DEFINITELY_UNSET_VAR_12345".into()
        }
    );
}

#[test]
fn from_env_present_var_resolves_and_reveals() {
    // SAFETY: test-only env mutation, unique var name per test.
    unsafe {
        std::env::set_var("STELLA_TEST_CREDENTIAL_VAR", "sk-test-value");
    }
    let key = ApiKey::from_env("STELLA_TEST_CREDENTIAL_VAR").unwrap();
    assert_eq!(key.reveal(), "sk-test-value");
    unsafe {
        std::env::remove_var("STELLA_TEST_CREDENTIAL_VAR");
    }
}

#[test]
fn from_env_empty_var_is_reported_as_empty_not_not_found() {
    unsafe {
        std::env::set_var("STELLA_TEST_EMPTY_CREDENTIAL_VAR", "");
    }
    let err = ApiKey::from_env("STELLA_TEST_EMPTY_CREDENTIAL_VAR").unwrap_err();
    assert_eq!(
        err,
        CredentialError::Empty {
            env_var: "STELLA_TEST_EMPTY_CREDENTIAL_VAR".into()
        }
    );
    unsafe {
        std::env::remove_var("STELLA_TEST_EMPTY_CREDENTIAL_VAR");
    }
}

/// The twin of `debug_never_prints_the_secret_value` for [`ApiKey`]:
/// `CredentialsFile` holds every configured provider key in plaintext, so
/// a derived `Debug` would dump the whole file into any log line, trace
/// record, or panic message that formats it.
#[test]
fn debug_never_prints_a_stored_key() {
    let mut file = CredentialsFile::empty();
    file.set("zai", "sk-zai-super-secret-value");
    file.set("anthropic", "sk-ant-super-secret-value");
    let debug = format!("{file:?}");
    assert!(!debug.contains("sk-zai-super-secret-value"), "{debug}");
    assert!(!debug.contains("sk-ant-super-secret-value"), "{debug}");
    assert!(debug.contains("redacted"), "{debug}");
    assert!(debug.contains('2'), "reports the provider count: {debug}");
}

#[test]
fn missing_file_loads_as_empty_not_an_error() {
    let path = temp_credentials_path("missing");
    let _ = std::fs::remove_file(&path);
    let file = CredentialsFile::load(&path).expect("missing file is not an error");
    assert_eq!(file.get("zai"), None);
}

#[test]
fn set_then_save_then_load_round_trips() {
    let path = temp_credentials_path("roundtrip");
    let _ = std::fs::remove_file(&path);

    let mut file = CredentialsFile::load(&path).unwrap();
    file.set("zai", "sk-zai-secret");
    file.set("anthropic", "sk-ant-secret");
    file.save().expect("save should succeed");

    let reloaded = CredentialsFile::load(&path).unwrap();
    assert_eq!(reloaded.get("zai"), Some("sk-zai-secret"));
    assert_eq!(reloaded.get("anthropic"), Some("sk-ant-secret"));
    assert_eq!(reloaded.get("openai"), None);

    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn saved_file_is_owner_read_write_only() {
    use std::os::unix::fs::PermissionsExt;
    let path = temp_credentials_path("perms");
    let _ = std::fs::remove_file(&path);

    let mut file = CredentialsFile::load(&path).unwrap();
    file.set("zai", "sk-secret");
    file.save().unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "credentials file must be 0600");

    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn overwriting_existing_credentials_stays_0600_and_leaves_no_temp_file() {
    use std::os::unix::fs::PermissionsExt;
    let path = temp_credentials_path("overwrite");
    let _ = std::fs::remove_file(&path);

    // First save creates the file.
    let mut file = CredentialsFile::load(&path).unwrap();
    file.set("zai", "sk-first");
    file.save().unwrap();

    // Second save must atomically replace it, keep 0600 (from the temp
    // file's birth mode, carried through the rename), and update content.
    file.set("zai", "sk-second-longer-value");
    file.save().unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "overwrite must preserve 0600");

    let reloaded = CredentialsFile::load(&path).unwrap();
    assert_eq!(reloaded.get("zai"), Some("sk-second-longer-value"));

    // No `.tmp.<pid>` sibling left behind after a successful rename.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    assert!(
        !tmp.exists(),
        "temp file must be renamed away, not left on disk"
    );

    let _ = std::fs::remove_file(&path);
}

/// The posture decision, pinned: a group/world-readable credentials file
/// is **read** (refusing would lock the user out of their own keys) and
/// **not chmod'ed** (the mode of a file we did not create is not ours to
/// change) — it raises an advisory instead.
#[cfg(unix)]
#[test]
fn a_loose_mode_credentials_file_loads_with_an_advisory_and_is_not_repaired() {
    use std::os::unix::fs::PermissionsExt;
    let path = temp_credentials_path("loose-mode");
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, "[credentials]\nzai = \"sk-zai-secret\"\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let file = CredentialsFile::load(&path).expect("a loose mode must never fail the read");
    assert_eq!(
        file.get("zai"),
        Some("sk-zai-secret"),
        "the keys must still resolve — warn, do not refuse"
    );
    assert_eq!(
        file.advisories(),
        [CredentialAdvisory::LoosePermissions {
            path: path.display().to_string(),
            mode: 0o644,
        }]
    );
    let line = file.advisories()[0].line();
    assert!(line.contains("chmod 600"), "{line}");
    assert!(
        !line.contains("sk-zai-secret"),
        "advisory must not echo the secret: {line}"
    );

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o644,
        "loading must never silently chmod a user's file"
    );

    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn an_owner_only_credentials_file_raises_no_advisory() {
    let path = temp_credentials_path("tight-mode");
    let _ = std::fs::remove_file(&path);
    let mut file = CredentialsFile::load(&path).unwrap();
    file.set("zai", "sk-zai-secret");
    file.save().unwrap();

    let reloaded = CredentialsFile::load(&path).unwrap();
    assert!(
        reloaded.advisories().is_empty(),
        "a file we wrote at 0600 is not worth warning about: {:?}",
        reloaded.advisories()
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_missing_credentials_file_raises_no_advisory() {
    let path = temp_credentials_path("absent-advisory");
    let _ = std::fs::remove_file(&path);
    let file = CredentialsFile::load(&path).unwrap();
    assert!(file.advisories().is_empty());
}

#[test]
fn malformed_toml_is_a_named_parse_error() {
    let path = temp_credentials_path("malformed");
    std::fs::write(&path, "this is not valid toml {{{").unwrap();
    let err = CredentialsFile::load(&path).unwrap_err();
    assert!(matches!(err, CredentialError::FileParse { .. }));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn remove_reports_whether_an_entry_existed_and_drops_it() {
    let path = temp_credentials_path("remove");
    let _ = std::fs::remove_file(&path);
    let mut file = CredentialsFile::load(&path).unwrap();
    file.set("zai", "sk-zai-secret");

    assert!(file.remove("zai"), "removing a present entry reports true");
    assert_eq!(file.get("zai"), None);
    assert!(
        !file.remove("zai"),
        "removing an already-absent entry reports false, not a panic"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn provider_ids_lists_every_stored_id_alphabetically() {
    let path = temp_credentials_path("provider-ids");
    let _ = std::fs::remove_file(&path);
    let mut file = CredentialsFile::load(&path).unwrap();
    file.set("zai", "sk-1");
    file.set("anthropic", "sk-2");
    file.set("openai", "sk-3");

    let ids: Vec<&str> = file.provider_ids().collect();
    assert_eq!(ids, vec!["anthropic", "openai", "zai"]);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn empty_file_has_no_entries_and_refuses_to_save() {
    let file = CredentialsFile::empty();
    assert_eq!(file.get("zai"), None);
    assert_eq!(file.provider_ids().count(), 0);
    assert!(matches!(
        file.save().unwrap_err(),
        CredentialError::FileWrite { .. }
    ));
}

// Each test below exercises the chain for a distinct provider id/env
// var pair so tests can run in parallel without racing on shared env
// state.

#[test]
fn resolve_cli_flag_wins_over_everything_else() {
    unsafe {
        std::env::set_var("STELLA_TEST_R1_KEY", "from-env");
    }
    let mut file = CredentialsFile::load(temp_credentials_path("r1")).unwrap();
    file.set("r1", "from-file");

    let (key, source) = ApiKey::resolve(
        "r1",
        "STELLA_TEST_R1_KEY",
        Some("from-flag"),
        Some(&file),
        false,
    )
    .unwrap();
    assert_eq!(key.reveal(), "from-flag");
    assert_eq!(source, CredentialSource::CliFlag);

    unsafe {
        std::env::remove_var("STELLA_TEST_R1_KEY");
    }
}

#[test]
fn resolve_env_var_wins_over_config_file() {
    unsafe {
        std::env::set_var("STELLA_TEST_R2_KEY", "from-env");
    }
    let mut file = CredentialsFile::load(temp_credentials_path("r2")).unwrap();
    file.set("r2", "from-file");

    let (key, source) =
        ApiKey::resolve("r2", "STELLA_TEST_R2_KEY", None, Some(&file), false).unwrap();
    assert_eq!(key.reveal(), "from-env");
    assert_eq!(source, CredentialSource::EnvVar);

    unsafe {
        std::env::remove_var("STELLA_TEST_R2_KEY");
    }
}

#[test]
fn resolve_falls_through_to_config_file_when_no_flag_or_env() {
    unsafe {
        std::env::remove_var("STELLA_TEST_R3_KEY");
    }
    let mut file = CredentialsFile::load(temp_credentials_path("r3")).unwrap();
    file.set("r3", "from-file");

    let (key, source) =
        ApiKey::resolve("r3", "STELLA_TEST_R3_KEY", None, Some(&file), false).unwrap();
    assert_eq!(key.reveal(), "from-file");
    assert_eq!(source, CredentialSource::ConfigFile);
}

#[test]
fn resolve_with_nothing_configured_and_non_interactive_is_a_named_not_found() {
    unsafe {
        std::env::remove_var("STELLA_TEST_R4_KEY");
    }
    let err = ApiKey::resolve("r4", "STELLA_TEST_R4_KEY", None, None, false).unwrap_err();
    assert_eq!(
        err,
        CredentialError::NotFound {
            env_var: "STELLA_TEST_R4_KEY".into()
        }
    );
}

#[test]
fn resolve_never_prompts_when_interactive_is_true_but_stdin_is_not_a_terminal() {
    // Test harnesses never run with a real TTY on stdin, so this also
    // guards against the suite hanging on a read that would otherwise
    // block forever waiting for input that will never arrive.
    unsafe {
        std::env::remove_var("STELLA_TEST_R5_KEY");
    }
    let err = ApiKey::resolve("r5", "STELLA_TEST_R5_KEY", None, None, true).unwrap_err();
    assert_eq!(
        err,
        CredentialError::NotFound {
            env_var: "STELLA_TEST_R5_KEY".into()
        }
    );
}

#[test]
fn resolve_empty_env_var_is_reported_as_empty_not_a_silent_fall_through() {
    // An explicitly-set-but-empty env var is a user mistake worth
    // surfacing distinctly, not something that should silently fall
    // through to the config file as if the var were merely unset.
    unsafe {
        std::env::set_var("STELLA_TEST_R6_KEY", "");
    }
    let err = ApiKey::resolve("r6", "STELLA_TEST_R6_KEY", None, None, false).unwrap_err();
    assert_eq!(
        err,
        CredentialError::Empty {
            env_var: "STELLA_TEST_R6_KEY".into()
        }
    );
    unsafe {
        std::env::remove_var("STELLA_TEST_R6_KEY");
    }
}
