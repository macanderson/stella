//! The scripted `GitCli` fake, shared by `git.rs`'s own tests and
//! `gc/tests.rs` — deduplicated so a new `GitCli` method needs one edit
//! rather than two identical ones.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::*;

type GitHandler = Box<dyn Fn(&[String]) -> GitOutput + Send + Sync>;

/// A [`GitCli`] that records every invocation and answers via a handler
/// closure — the seam that lets tests assert on the exact git arguments
/// (esp. the pathspec discipline) without a real repo.
pub(crate) struct ScriptedGit {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    handler: GitHandler,
}

impl ScriptedGit {
    pub(crate) fn new(handler: impl Fn(&[String]) -> GitOutput + Send + Sync + 'static) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            handler: Box::new(handler),
        }
    }

    pub(crate) fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl GitCli for ScriptedGit {
    async fn run(&self, _repo: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(owned.clone());
        Ok((self.handler)(&owned))
    }
}
