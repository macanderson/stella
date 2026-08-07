use super::*;

#[test]
fn pipeline_test_command_uses_central_credential_scrub_policy() {
    let mut command = tokio::process::Command::new("sh");
    command
        .env("OPENROUTER_API_KEY", "provider-secret")
        .env("GITHUB_TOKEN", "repo-secret")
        .env("AWS_SECRET_ACCESS_KEY", "cloud-secret")
        .env("STELLA_TEST_BENIGN", "visible");

    scrub_model_subprocess(&mut command);

    let overrides: std::collections::HashMap<_, _> = command
        .as_std()
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.map(std::ffi::OsStr::to_os_string),
            )
        })
        .collect();
    for secret in [
        "OPENROUTER_API_KEY",
        "GITHUB_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
    ] {
        assert_eq!(
            overrides.get(secret),
            Some(&None),
            "{secret} was not removed"
        );
    }
    assert_eq!(
        overrides["STELLA_TEST_BENIGN"].as_deref(),
        Some(std::ffi::OsStr::new("visible"))
    );
}
