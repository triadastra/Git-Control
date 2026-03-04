use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local};
use git2::{
    build::CheckoutBuilder, BranchType, Commit, Oid, Repository, ResetType, Sort, Status,
    StatusOptions,
};
use walkdir::WalkDir;

#[derive(Debug, Clone, Default)]
pub struct RepoSummary {
    pub name: String,
    pub path: PathBuf,
    pub current_branch: String,
    pub staged_count: usize,
    pub unstaged_count: usize,
    pub untracked_count: usize,
    pub conflict_count: usize,
    pub ahead: usize,
    pub behind: usize,
    pub next_step: String,
}

#[derive(Debug, Clone, Default)]
pub struct RepoSnapshot {
    pub summary: RepoSummary,
    pub changes: Vec<FileChange>,
    pub commits: Vec<CommitEntry>,
    pub branches: Vec<BranchEntry>,
    pub conflicts: Vec<ConflictEntry>,
    pub recovery: Vec<RecoveryEntry>,
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    pub kind: String,
    pub staged: bool,
    pub unstaged: bool,
}

#[derive(Debug, Clone)]
pub struct CommitEntry {
    pub oid: String,
    pub id: String,
    pub summary: String,
    pub author: String,
    pub timestamp: String,
    pub parents: Vec<String>,
    pub branch_labels: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BranchEntry {
    pub name: String,
    pub is_head: bool,
    pub upstream: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConflictEntry {
    pub path: String,
    pub ours_label: String,
    pub theirs_label: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct RecoveryEntry {
    pub to_id: String,
    pub to_id_short: String,
    pub from_id_short: String,
    pub message: String,
    pub timestamp: String,
}

pub struct GitService;

impl GitService {
    pub fn resolve_existing_repo(path: &Path) -> Result<PathBuf> {
        let repo = Repository::discover(path).with_context(|| {
            format!(
                "no existing git repository found at or above {}",
                path.display()
            )
        })?;
        Ok(repo_root_from_repo(&repo))
    }

    pub fn discover_repositories(root: &Path, max_depth: usize) -> Vec<PathBuf> {
        let mut repos = Vec::new();

        // Walk the directory tree looking for .git folders
        for entry in WalkDir::new(root)
            .max_depth(max_depth)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            // Skip .git directories themselves (don't descend into them)
            if entry.file_name() == ".git" && entry.file_type().is_dir() {
                if let Some(parent) = entry.path().parent() {
                    // Verify it's actually a valid git repo
                    match Repository::open(parent) {
                        Ok(_) => {
                            repos.push(parent.to_path_buf());
                        }
                        Err(_) => {
                            // Not a valid repo, skip it
                        }
                    }
                }
            }
        }

        repos.sort();
        repos.dedup();
        repos
    }

    pub fn load_summary(repo_path: &Path) -> Result<RepoSummary> {
        let repo = Repository::open(repo_path)
            .with_context(|| format!("failed to open repository {}", repo_path.display()))?;

        let name = repo_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_owned();

        let current_branch = current_branch_name(&repo).unwrap_or_else(|| "detached".to_owned());

        let (ahead, behind) = ahead_behind(&repo)?;

        let mut status_opts = StatusOptions::new();
        status_opts
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .renames_head_to_index(true);

        let statuses = repo.statuses(Some(&mut status_opts))?;

        let mut staged_count = 0;
        let mut unstaged_count = 0;
        let mut untracked_count = 0;
        let mut conflict_count = 0;

        for status in statuses.iter() {
            let bits = status.status();
            if bits.is_conflicted() {
                conflict_count += 1;
            }
            if is_staged(bits) {
                staged_count += 1;
            }
            if is_unstaged(bits) {
                unstaged_count += 1;
            }
            if bits.contains(Status::WT_NEW) {
                untracked_count += 1;
            }
        }

        let next_step = next_step_hint(conflict_count, staged_count, unstaged_count, ahead);

        Ok(RepoSummary {
            name,
            path: repo_path.to_path_buf(),
            current_branch,
            staged_count,
            unstaged_count,
            untracked_count,
            conflict_count,
            ahead,
            behind,
            next_step,
        })
    }

    pub fn load_snapshot(repo_path: &Path) -> Result<RepoSnapshot> {
        let repo = Repository::open(repo_path)
            .with_context(|| format!("failed to open repository {}", repo_path.display()))?;

        let summary = Self::load_summary(repo_path)?;
        let changes = collect_changes(&repo)?;
        let commits = collect_commits(&repo, 120)?;
        let branches = collect_branches(&repo)?;
        let conflicts = collect_conflicts(&repo)?;
        let recovery = collect_recovery(&repo)?;

        Ok(RepoSnapshot {
            summary,
            changes,
            commits,
            branches,
            conflicts,
            recovery,
        })
    }

    pub fn stage_all(repo_path: &Path) -> Result<()> {
        let repo = Repository::open(repo_path)?;
        let workdir = repo
            .workdir()
            .context("bare repositories are not supported")?
            .to_path_buf();
        let mut index = repo.index()?;

        let mut status_opts = StatusOptions::new();
        status_opts
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .renames_head_to_index(true);
        let statuses = repo.statuses(Some(&mut status_opts))?;

        for entry in statuses.iter() {
            let status = entry.status();
            let Some(path) = entry.path() else {
                continue;
            };

            if status.contains(Status::WT_DELETED) {
                let _ = index.remove_path(Path::new(path));
                continue;
            }

            let full = workdir.join(path);
            if full.exists() {
                index.add_path(Path::new(path))?;
            }
        }

        index.write()?;
        Ok(())
    }

    pub fn stage_path(repo_path: &Path, rel_path: &str) -> Result<()> {
        let repo = Repository::open(repo_path)?;
        let workdir = repo
            .workdir()
            .context("bare repositories are not supported")?
            .to_path_buf();
        let mut index = repo.index()?;
        let full = workdir.join(rel_path);
        if full.exists() {
            index.add_path(Path::new(rel_path))?;
        } else {
            let _ = index.remove_path(Path::new(rel_path));
        }
        index.write()?;
        Ok(())
    }

    pub fn unstage_all(repo_path: &Path) -> Result<()> {
        let repo = Repository::open(repo_path)?;
        match head_target_object(&repo) {
            Some(obj) => repo.reset_default(Some(&obj), std::iter::empty::<&str>())?,
            None => {
                let mut index = repo.index()?;
                index.clear()?;
                index.write()?;
            }
        }
        Ok(())
    }

    pub fn unstage_path(repo_path: &Path, rel_path: &str) -> Result<()> {
        let repo = Repository::open(repo_path)?;
        match head_target_object(&repo) {
            Some(obj) => repo.reset_default(Some(&obj), [rel_path])?,
            None => {
                let mut index = repo.index()?;
                let _ = index.remove_path(Path::new(rel_path));
                index.write()?;
            }
        }
        Ok(())
    }

    pub fn commit(repo_path: &Path, message: &str) -> Result<String> {
        let repo = Repository::open(repo_path)?;
        let mut index = repo.index()?;

        if !has_staged_changes(&repo)? {
            return Err(anyhow!("nothing staged to commit"));
        }

        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;

        let sig = repo
            .signature()
            .or_else(|_| git2::Signature::now("Git Control", "git-control@local"))
            .context("failed to create commit signature")?;

        let mut parents: Vec<Commit<'_>> = Vec::new();
        if let Ok(head_ref) = repo.head() {
            if let Some(head_id) = head_ref.target() {
                parents.push(repo.find_commit(head_id)?);
            }
        }

        let parent_refs: Vec<&Commit<'_>> = parents.iter().collect();
        let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)?;
        Ok(oid.to_string())
    }

    pub fn create_branch(repo_path: &Path, name: &str, checkout: bool) -> Result<()> {
        let repo = Repository::open(repo_path)?;
        let head_commit = repo.head()?.peel_to_commit()?;
        repo.branch(name, &head_commit, false)?;

        if checkout {
            Self::checkout_branch(repo_path, name)?;
        }

        Ok(())
    }

    pub fn checkout_branch(repo_path: &Path, name: &str) -> Result<()> {
        let repo = Repository::open(repo_path)?;
        let reference = format!("refs/heads/{name}");
        let obj = repo.revparse_single(&reference)?;
        repo.checkout_tree(&obj, Some(CheckoutBuilder::new().safe()))?;
        repo.set_head(&reference)?;
        Ok(())
    }

    pub fn apply_resolution(repo_path: &Path, rel_file: &str, resolved_text: &str) -> Result<()> {
        let repo = Repository::open(repo_path)?;
        let workdir = repo
            .workdir()
            .context("bare repository is not supported for conflict resolution")?;
        let full = workdir.join(rel_file);
        fs::write(&full, resolved_text)
            .with_context(|| format!("failed to write resolution to {}", full.display()))?;

        let mut index = repo.index()?;
        index.add_path(Path::new(rel_file))?;
        index.write()?;

        Ok(())
    }

    pub fn mixed_reset_to(repo_path: &Path, oid: &str) -> Result<()> {
        let repo = Repository::open(repo_path)?;
        let parsed = Oid::from_str(oid)?;
        let obj = repo.find_object(parsed, None)?;
        repo.reset(&obj, ResetType::Mixed, None)?;
        Ok(())
    }

    pub fn fetch(repo_path: &Path) -> Result<String> {
        run_git_command(repo_path, &["fetch", "--all", "--prune"])
    }

    pub fn pull_rebase(repo_path: &Path) -> Result<String> {
        run_git_command(repo_path, &["pull", "--rebase"])
    }

    pub fn push(repo_path: &Path) -> Result<String> {
        run_git_command(repo_path, &["push"])
    }
}

fn current_branch_name(repo: &Repository) -> Option<String> {
    let head = repo.head().ok()?;
    if !head.is_branch() {
        return None;
    }
    head.shorthand().map(str::to_owned)
}

fn repo_root_from_repo(repo: &Repository) -> PathBuf {
    if let Some(workdir) = repo.workdir() {
        return workdir.to_path_buf();
    }

    let git_dir = repo.path();
    if git_dir.file_name().and_then(|n| n.to_str()) == Some(".git") {
        if let Some(parent) = git_dir.parent() {
            return parent.to_path_buf();
        }
    }

    git_dir.to_path_buf()
}

fn head_target_object(repo: &Repository) -> Option<git2::Object<'_>> {
    let head = repo.head().ok()?;
    let oid = head.target()?;
    repo.find_object(oid, None).ok()
}

fn ahead_behind(repo: &Repository) -> Result<(usize, usize)> {
    let head = match repo.head() {
        Ok(head) => head,
        Err(_) => return Ok((0, 0)),
    };

    if !head.is_branch() {
        return Ok((0, 0));
    }

    let head_oid = match head.target() {
        Some(id) => id,
        None => return Ok((0, 0)),
    };

    let head_name = head.shorthand().unwrap_or_default();
    let branch = repo.find_branch(head_name, BranchType::Local)?;
    let upstream = match branch.upstream() {
        Ok(up) => up,
        Err(_) => return Ok((0, 0)),
    };

    let upstream_oid = match upstream.get().target() {
        Some(id) => id,
        None => return Ok((0, 0)),
    };

    let (ahead, behind) = repo.graph_ahead_behind(head_oid, upstream_oid)?;
    Ok((ahead, behind))
}

fn collect_changes(repo: &Repository) -> Result<Vec<FileChange>> {
    let mut status_opts = StatusOptions::new();
    status_opts
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);

    let statuses = repo.statuses(Some(&mut status_opts))?;
    let mut changes = Vec::new();

    for entry in statuses.iter() {
        let status = entry.status();
        let path = entry.path().unwrap_or("<unknown>").to_owned();
        changes.push(FileChange {
            path,
            kind: describe_status(status),
            staged: is_staged(status),
            unstaged: is_unstaged(status),
        });
    }

    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

fn collect_commits(repo: &Repository, max: usize) -> Result<Vec<CommitEntry>> {
    let branch_labels = build_branch_labels_map(repo)?;
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;

    if revwalk.push_head().is_err() {
        return Ok(Vec::new());
    }

    let mut commits = Vec::new();
    for oid in revwalk.take(max).flatten() {
        let commit = repo.find_commit(oid)?;
        let parents = commit.parent_ids().map(|id| id.to_string()).collect();
        let mut labels = branch_labels.get(&oid).cloned().unwrap_or_default();
        labels.sort();
        commits.push(CommitEntry {
            oid: commit.id().to_string(),
            id: short_id(commit.id()),
            summary: commit.summary().unwrap_or("(no message)").to_owned(),
            author: commit.author().name().unwrap_or("unknown").to_owned(),
            timestamp: format_git_time(commit.time().seconds()),
            parents,
            branch_labels: labels,
        });
    }

    Ok(commits)
}

fn build_branch_labels_map(repo: &Repository) -> Result<HashMap<Oid, Vec<String>>> {
    let mut map: HashMap<Oid, Vec<String>> = HashMap::new();
    for reference in repo.references()? {
        let reference = reference?;
        let Some(name) = reference.name() else {
            continue;
        };
        if !name.starts_with("refs/heads/") && !name.starts_with("refs/remotes/") {
            continue;
        }
        let Some(target) = reference.target() else {
            continue;
        };
        let label = name
            .trim_start_matches("refs/heads/")
            .trim_start_matches("refs/remotes/")
            .to_owned();
        map.entry(target).or_default().push(label);
    }
    Ok(map)
}

fn collect_branches(repo: &Repository) -> Result<Vec<BranchEntry>> {
    let mut out = Vec::new();
    for branch_result in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch_result?;
        let name = branch
            .name()
            .ok()
            .flatten()
            .unwrap_or("<invalid>")
            .to_owned();
        let upstream = branch
            .upstream()
            .ok()
            .and_then(|up| up.name().ok().flatten().map(str::to_owned))
            .map(|name| name.replace("refs/remotes/", ""));

        out.push(BranchEntry {
            name,
            is_head: branch.is_head(),
            upstream,
        });
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn collect_conflicts(repo: &Repository) -> Result<Vec<ConflictEntry>> {
    let mut out = Vec::new();
    let index = repo.index()?;

    if !index.has_conflicts() {
        return Ok(out);
    }

    let workdir = repo
        .workdir()
        .context("bare repositories are not supported")?
        .to_path_buf();

    for conflict in index.conflicts()? {
        let conflict = conflict?;
        let path = conflict
            .our
            .as_ref()
            .or(conflict.their.as_ref())
            .or(conflict.ancestor.as_ref())
            .map(|entry| String::from_utf8_lossy(&entry.path).to_string())
            .unwrap_or_else(|| "<unknown>".to_owned());

        let ours_label = conflict
            .our
            .as_ref()
            .and_then(|_| current_branch_name(repo))
            .unwrap_or_else(|| "ours".to_owned());

        let theirs_label = conflict
            .their
            .as_ref()
            .map(|_| "incoming".to_owned())
            .unwrap_or_else(|| "theirs".to_owned());

        let content = fs::read_to_string(workdir.join(&path)).unwrap_or_default();

        out.push(ConflictEntry {
            path,
            ours_label,
            theirs_label,
            content,
        });
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn collect_recovery(repo: &Repository) -> Result<Vec<RecoveryEntry>> {
    let reflog = match repo.reflog("HEAD") {
        Ok(log) => log,
        Err(_) => return Ok(Vec::new()),
    };

    let mut entries = Vec::new();

    for idx in (0..reflog.len()).rev().take(60) {
        if let Some(entry) = reflog.get(idx) {
            let ts = format_git_time(entry.committer().when().seconds());
            let message = entry.message().unwrap_or("(no reflog message)").to_owned();
            let to_id = entry.id_new().to_string();
            entries.push(RecoveryEntry {
                to_id_short: short_id(entry.id_new()),
                from_id_short: short_id(entry.id_old()),
                to_id,
                message,
                timestamp: ts,
            });
        }
    }

    Ok(entries)
}

fn short_id(oid: Oid) -> String {
    let full = oid.to_string();
    full.chars().take(8).collect()
}

fn describe_status(status: Status) -> String {
    if status.is_conflicted() {
        return "conflicted".to_owned();
    }

    if status.contains(Status::WT_NEW) || status.contains(Status::INDEX_NEW) {
        return "new".to_owned();
    }

    if status.contains(Status::WT_MODIFIED) || status.contains(Status::INDEX_MODIFIED) {
        return "modified".to_owned();
    }

    if status.contains(Status::WT_DELETED) || status.contains(Status::INDEX_DELETED) {
        return "deleted".to_owned();
    }

    if status.contains(Status::WT_RENAMED) || status.contains(Status::INDEX_RENAMED) {
        return "renamed".to_owned();
    }

    if status.contains(Status::WT_TYPECHANGE) || status.contains(Status::INDEX_TYPECHANGE) {
        return "typechange".to_owned();
    }

    "changed".to_owned()
}

fn is_staged(status: Status) -> bool {
    status.intersects(
        Status::INDEX_NEW
            | Status::INDEX_MODIFIED
            | Status::INDEX_DELETED
            | Status::INDEX_RENAMED
            | Status::INDEX_TYPECHANGE,
    )
}

fn is_unstaged(status: Status) -> bool {
    status.intersects(
        Status::WT_MODIFIED
            | Status::WT_DELETED
            | Status::WT_RENAMED
            | Status::WT_TYPECHANGE
            | Status::WT_NEW,
    )
}

fn format_git_time(seconds: i64) -> String {
    if let Some(ts) = DateTime::from_timestamp(seconds, 0) {
        ts.with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    } else {
        "unknown".to_owned()
    }
}

fn next_step_hint(conflicts: usize, staged: usize, unstaged: usize, ahead: usize) -> String {
    if conflicts > 0 {
        "Resolve merge conflicts in Conflict Studio before committing.".to_owned()
    } else if unstaged > 0 {
        "Review changes and stage what belongs in your next commit.".to_owned()
    } else if staged > 0 {
        "Write a clear commit message and create your commit.".to_owned()
    } else if ahead > 0 {
        "Branch is ahead of remote. Sync by pushing when ready.".to_owned()
    } else {
        "Working tree is clean. Start a branch or pull latest updates.".to_owned()
    }
}

fn run_git_command(repo_path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();

    if output.status.success() {
        if stdout.is_empty() && stderr.is_empty() {
            Ok(format!("git {} completed successfully", args.join(" ")))
        } else if stdout.is_empty() {
            Ok(stderr)
        } else if stderr.is_empty() {
            Ok(stdout)
        } else {
            Ok(format!("{stdout}\n{stderr}"))
        }
    } else {
        let details = if stdout.is_empty() && stderr.is_empty() {
            "command failed without output".to_owned()
        } else if stdout.is_empty() {
            stderr
        } else if stderr.is_empty() {
            stdout
        } else {
            format!("{stdout}\n{stderr}")
        };
        Err(anyhow!("git {} failed: {}", args.join(" "), details))
    }
}

fn has_staged_changes(repo: &Repository) -> Result<bool> {
    let mut status_opts = StatusOptions::new();
    status_opts
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true);

    let statuses = repo.statuses(Some(&mut status_opts))?;
    Ok(statuses.iter().any(|s| is_staged(s.status())))
}

#[cfg(test)]
mod tests {
    use super::{has_staged_changes, GitService};
    use anyhow::Result;
    use git2::{Oid, Repository, Signature};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn unstage_all_works_without_head() -> Result<()> {
        let repo_dir = temp_repo_dir("unstage_all_no_head");
        let repo = Repository::init(&repo_dir)?;
        write_file(repo_dir.join("a.txt"), "hello")?;

        GitService::stage_path(&repo_dir, "a.txt")?;
        assert!(has_staged_changes(&repo)?);

        GitService::unstage_all(&repo_dir)?;
        assert!(!has_staged_changes(&repo)?);

        cleanup(&repo_dir);
        Ok(())
    }

    #[test]
    fn unstage_path_works_without_head() -> Result<()> {
        let repo_dir = temp_repo_dir("unstage_path_no_head");
        let repo = Repository::init(&repo_dir)?;
        write_file(repo_dir.join("a.txt"), "a")?;
        write_file(repo_dir.join("b.txt"), "b")?;

        GitService::stage_path(&repo_dir, "a.txt")?;
        GitService::stage_path(&repo_dir, "b.txt")?;
        GitService::unstage_path(&repo_dir, "a.txt")?;

        let index = repo.index()?;
        let paths: Vec<String> = index
            .iter()
            .map(|entry| String::from_utf8_lossy(&entry.path).to_string())
            .collect();
        assert_eq!(paths, vec!["b.txt".to_owned()]);

        cleanup(&repo_dir);
        Ok(())
    }

    #[test]
    fn stage_deleted_file_and_commit() -> Result<()> {
        let repo_dir = temp_repo_dir("stage_deleted_file");
        let repo = Repository::init(&repo_dir)?;
        write_file(repo_dir.join("file.txt"), "v1")?;
        commit_all(&repo, "initial")?;

        fs::remove_file(repo_dir.join("file.txt"))?;
        GitService::stage_path(&repo_dir, "file.txt")?;
        let _ = GitService::commit(&repo_dir, "delete file")?;

        let head = repo.head()?.peel_to_commit()?;
        let tree = head.tree()?;
        assert!(tree.get_path(Path::new("file.txt")).is_err());

        cleanup(&repo_dir);
        Ok(())
    }

    #[test]
    fn stage_all_handles_new_and_deleted_files() -> Result<()> {
        let repo_dir = temp_repo_dir("stage_all_mixed");
        let repo = Repository::init(&repo_dir)?;
        write_file(repo_dir.join("old.txt"), "old")?;
        commit_all(&repo, "initial")?;

        fs::remove_file(repo_dir.join("old.txt"))?;
        write_file(repo_dir.join("new.txt"), "new")?;

        GitService::stage_all(&repo_dir)?;
        let _ = GitService::commit(&repo_dir, "replace file")?;

        let head = repo.head()?.peel_to_commit()?;
        let tree = head.tree()?;
        assert!(tree.get_path(Path::new("new.txt")).is_ok());
        assert!(tree.get_path(Path::new("old.txt")).is_err());

        cleanup(&repo_dir);
        Ok(())
    }

    #[test]
    fn commit_requires_staged_changes() -> Result<()> {
        let repo_dir = temp_repo_dir("commit_needs_staged");
        let repo = Repository::init(&repo_dir)?;
        write_file(repo_dir.join("file.txt"), "v1")?;
        commit_all(&repo, "initial")?;

        let err = GitService::commit(&repo_dir, "noop").unwrap_err();
        assert!(err.to_string().contains("nothing staged"));

        cleanup(&repo_dir);
        Ok(())
    }

    #[test]
    fn resolve_existing_repo_finds_parent_repo_from_subdir() -> Result<()> {
        let repo_dir = temp_repo_dir("resolve_parent_repo");
        let _repo = Repository::init(&repo_dir)?;
        let nested = repo_dir.join("nested").join("deeper");
        fs::create_dir_all(&nested)?;

        let resolved = GitService::resolve_existing_repo(&nested)?;
        assert_eq!(resolved.canonicalize()?, repo_dir.canonicalize()?);

        cleanup(&repo_dir);
        Ok(())
    }

    #[test]
    fn resolve_existing_repo_does_not_initialize_when_missing() -> Result<()> {
        let dir = temp_repo_dir("resolve_missing_repo");
        let target = dir.join("not-a-repo");
        fs::create_dir_all(&target)?;

        let err = GitService::resolve_existing_repo(&target).unwrap_err();
        assert!(err.to_string().contains("no existing git repository found"));
        assert!(!target.join(".git").exists());

        cleanup(&dir);
        Ok(())
    }

    fn temp_repo_dir(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "git_control_{}_{}_{}",
            prefix,
            std::process::id(),
            now
        ));
        fs::create_dir_all(&dir).expect("create temp repo dir");
        dir
    }

    fn write_file(path: PathBuf, content: &str) -> Result<()> {
        fs::write(path, content)?;
        Ok(())
    }

    fn commit_all(repo: &Repository, message: &str) -> Result<Oid> {
        let mut index = repo.index()?;
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;
        let sig = Signature::now("Git Control Test", "git-control-test@local")?;

        let mut parents = Vec::new();
        if let Ok(head) = repo.head() {
            if let Some(oid) = head.target() {
                parents.push(repo.find_commit(oid)?);
            }
        }
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
        let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)?;
        Ok(oid)
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }
}
