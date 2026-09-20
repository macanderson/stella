//! The worker boundary keeps credentials private and survives a dropped session.

use super::*;

#[test]
fn the_pipe_preserves_resolved_configuration_without_environment_round_trips() {
    let env = EmbedderEnv {
        url: Some("http://127.0.0.1:1".into()),
        model: Some("fixture".into()),
        api_key: Some("pipe-only-fixture-secret".into()),
        dims: Some("2".into()),
        floor: Some("0.4".into()),
        voyage_api_key: Some("sealed-voyage-fixture".into()),
        openai_api_key: None,
    };
    let bytes = serde_json::to_vec(&Request {
        embedder: env.clone(),
    })
    .unwrap();
    let decoded: Request = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded.embedder, env);
}

#[cfg(unix)]
#[tokio::test]
async fn worker_has_its_own_session_and_survives_a_dropped_handle() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("worker.sh");
    std::fs::write(
        &program,
        r#"#!/bin/sh
set -eu
cat > request.json
[ -z "${STELLA_EMBED_API_KEY+x}" ]
printf ready > ready
n=0
while [ ! -f release ] && [ "$n" -lt 500 ]; do
    sleep 0.01
    n=$((n + 1))
done
[ -f release ] && printf complete > complete
"#,
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env = EmbedderEnv {
        api_key: Some("pipe-only-fixture-secret".into()),
        ..EmbedderEnv::default()
    };
    let mut child = spawn(&program, dir.path(), env.clone()).await.unwrap();
    let pid = i32::try_from(child.id().unwrap()).unwrap();
    // SAFETY: getsid only reads the OS session id for the live child.
    assert_eq!(
        unsafe { libc::getsid(pid) },
        pid,
        "the worker must leave the terminal's session"
    );
    wait_for(&dir.path().join("ready")).await;
    let decoded: Request =
        serde_json::from_slice(&std::fs::read(dir.path().join("request.json")).unwrap()).unwrap();
    assert_eq!(decoded.embedder, env);
    assert!(child.try_wait().unwrap().is_none());
    drop(child);
    std::fs::write(dir.path().join("release"), "go").unwrap();
    wait_for(&dir.path().join("complete")).await;
}

#[cfg(unix)]
async fn wait_for(path: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker reached the barrier");
}
