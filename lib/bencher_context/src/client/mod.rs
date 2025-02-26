use std::collections::HashMap;

use gix::Repository;

mod platform;

pub use platform::Fingerprint;
use platform::OperatingSystem;

use crate::ReportContext;

const ROOT: &str = "root";

impl ReportContext {
    pub fn new() -> Self {
        get_context()
    }

    fn insert(&mut self, path: &str, value: String) -> Option<String> {
        let key = format!("bencher.dev{path}");
        self.0.insert(key, value)
    }
}

#[allow(clippy::implicit_hasher)]
impl From<ReportContext> for HashMap<String, String> {
    fn from(context: ReportContext) -> Self {
        context.0
    }
}

pub fn get_context() -> ReportContext {
    let mut context = ReportContext::default();
    git_context(&mut context);
    platform_context(&mut context);
    context
}

fn git_context(context: &mut ReportContext) {
    let Some(repo) = find_repo() else {
        return;
    };

    if let Some(repo_name) = repo_name(&repo) {
        context.insert("/repo/name", repo_name);
    }

    if let Some(root_commit) = repo_hash(&repo) {
        context.insert("/repo/hash", root_commit);
    }

    if let Some((branch_ref, branch_ref_name)) = branch_ref(&repo) {
        context.insert("/branch/ref", branch_ref);
        context.insert("/branch/ref/name", branch_ref_name);
    }

    if let Some(hash) = branch_hash(&repo) {
        context.insert("/branch/hash", hash);
    }
}

fn platform_context(context: &mut ReportContext) {
    context.insert("/testbed/os", OperatingSystem::current().to_string());

    if let Some(fingerprint) = Fingerprint::current() {
        context.insert("/testbed/fingerprint", fingerprint.to_string());
    }
}

fn find_repo() -> Option<Repository> {
    let current_dir = std::env::current_dir().ok()?;
    for directory in current_dir.ancestors() {
        if let Ok(repo) = gix::open(directory) {
            return Some(repo);
        }
    }
    None
}

fn repo_name(repo: &Repository) -> Option<String> {
    let Some(parent) = repo.path().parent() else {
        return Some(ROOT.to_owned());
    };
    let file_name = parent.file_name()?;
    file_name.to_str().map(ToOwned::to_owned)
}

fn repo_hash(repo: &Repository) -> Option<String> {
    let head_id = repo.head_id().ok()?;
    let rev_walk = repo.rev_walk([head_id]).all().ok()?;
    if let Some(Ok(commit)) = rev_walk.last() {
        Some(commit.id().object().ok()?.id.to_string())
    } else {
        None
    }
}

fn branch_ref(repo: &Repository) -> Option<(String, String)> {
    repo.head().ok()?.referent_name().map(|name| {
        (
            String::from_utf8_lossy(name.as_bstr()).to_string(),
            String::from_utf8_lossy(name.shorten()).to_string(),
        )
    })
}

fn branch_hash(repo: &Repository) -> Option<String> {
    let head_id = repo.head_id().ok()?;
    let head_object = head_id.object().ok()?;
    Some(head_object.id.to_string())
}
