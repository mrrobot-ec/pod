use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::error::{PodError, Result};
use crate::herdr::{TabInfo, WorktreeCreate};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Worktree {
    #[default]
    Current,
    NewChild,
    NewTopLevel,
    Path(PathBuf),
    Branch(String),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Setup {
    #[default]
    Run,
    Skip,
    Inherit,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Placement {
    pub kind: Worktree,
    pub workspace_id: String,
    pub path: PathBuf,
    pub initial_tab: Option<TabInfo>,
    pub owns_initial_tab: bool,
    /// The repository a new worktree was created from (setup config lookup).
    pub repo: Option<PathBuf>,
    /// The workspaces herdr opened for this placement and nobody asked for:
    /// the repository's parent space when it was not open, and the worktree's
    /// own space when `path:` or `branch:` opened it. Release closes each
    /// that is left holding only the shell herdr gave it.
    pub opened_workspaces: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CreationOptions<'a> {
    pub name: Option<&'a str>,
    pub repo: Option<&'a str>,
    pub base_branch: Option<&'a str>,
    pub display_name: Option<&'a str>,
    pub comment: Option<&'a str>,
    pub setup_was_explicit: bool,
}

pub fn validate_creation_flags(worktree: &Worktree, options: &CreationOptions<'_>) -> Result<()> {
    let any = options.name.is_some()
        || options.repo.is_some()
        || options.base_branch.is_some()
        || options.display_name.is_some()
        || options.comment.is_some()
        || options.setup_was_explicit;
    if any && !matches!(worktree, Worktree::NewChild | Worktree::NewTopLevel) {
        return Err(PodError::usage(
            "worktree creation flags are valid only with new-child or new-top-level",
        ));
    }
    if matches!(worktree, Worktree::NewChild | Worktree::NewTopLevel) && options.name.is_none() {
        return Err(PodError::usage("--name is required for a new worktree"));
    }
    Ok(())
}

pub fn place(
    engine: &Engine,
    worktree: &Worktree,
    options: &CreationOptions<'_>,
    coordinator_workspace: &str,
    coordinator_path: &Path,
) -> Result<Placement> {
    validate_creation_flags(worktree, options)?;
    match worktree {
        Worktree::Current => Ok(Placement {
            kind: worktree.clone(),
            workspace_id: coordinator_workspace.to_owned(),
            path: coordinator_path.to_path_buf(),
            initial_tab: None,
            owns_initial_tab: false,
            repo: None,
            opened_workspaces: Vec::new(),
        }),
        Worktree::NewChild | Worktree::NewTopLevel => {
            let repo = source_checkout(
                engine,
                &resolve_repo(engine, options.repo, coordinator_path)?,
            )?;
            let name = options.name.expect("validated name");
            let configured = engine.config.repos.get(&repo);
            let base_branch = match worktree {
                Worktree::NewChild => None,
                Worktree::NewTopLevel => options
                    .base_branch
                    .map(str::to_owned)
                    .or_else(|| configured.and_then(|repo| repo.base_branch.clone())),
                _ => unreachable!(),
            };
            // The space's label names the repository too, so a worktree
            // created for another project reads `<repo>/<name>` in the sidebar.
            let display_name = options.display_name.map(str::to_owned).or_else(|| {
                repo.file_name()
                    .and_then(|folder| folder.to_str())
                    .map(|folder| format!("{folder}/{name}"))
            });
            let parent_before = engine.herdr.workspace_for_folder(&repo)?;
            let info = engine.herdr.worktree_create(&WorktreeCreate {
                repo: repo.clone(),
                name: name.to_owned(),
                base_branch,
                display_name,
                comment: options.comment.map(str::to_owned),
            })?;
            let opened_workspaces = parent_opened(engine, &repo, parent_before)?;
            Ok(Placement {
                kind: worktree.clone(),
                workspace_id: info.workspace_id,
                path: info.path,
                initial_tab: Some(TabInfo {
                    tab_id: info.tab_id,
                    pane_id: info.pane_id,
                }),
                owns_initial_tab: true,
                repo: Some(repo),
                opened_workspaces,
            })
        }
        Worktree::Path(path) => {
            // herdr lists the repository containing the folder it is given.
            // Asked without one it answers for its focused workspace,
            // whatever the person is looking at, which may be no repository
            // (seen live 2026-10-15: four starts failed with git's "not a git
            // repository" for the focused folder while every target was a
            // valid worktree).
            let listing = match engine.herdr.worktree_list(Some(path)) {
                Ok(listing) => Some(listing),
                // The target itself is no repository: a plain folder.
                Err(error) if error.message.contains("not a git repository") => None,
                Err(error) => {
                    return Err(at_step(
                        &format!("herdr worktree.list in {}", path.display()),
                        error,
                    ));
                }
            };
            match listing {
                Some(listing) if listing.worktrees.iter().any(|known| known.path == *path) => {
                    let source = listing.source_checkout.as_deref().unwrap_or(path);
                    opened(engine, worktree, path, source)
                }
                _ => folder(engine, worktree, path),
            }
        }
        Worktree::Branch(branch) => {
            let qualified = format!("refs/heads/{branch}");
            let listing = engine
                .herdr
                .worktree_list(Some(coordinator_path))
                .map_err(|error| {
                    at_step(
                        &format!("herdr worktree.list in {}", coordinator_path.display()),
                        error,
                    )
                })?;
            let path = listing
                .worktrees
                .iter()
                .find(|worktree| worktree.branch == *branch || worktree.branch == qualified)
                .map(|worktree| worktree.path.clone())
                .ok_or_else(|| {
                    PodError::usage(format!(
                        "no existing worktree was found for branch {branch}"
                    ))
                })?;
            let source = listing.source_checkout.as_deref().unwrap_or(&path);
            opened(engine, worktree, &path, source)
        }
    }
}

/// The folder herdr starts worktree actions from: the source checkout of the
/// repository containing `folder`, as herdr's own listing names it. herdr
/// refuses a linked worktree as the start (`linked_worktree_source`, seen
/// live 2026-10-15 for two starts whose target was a worktree of another
/// repository), so a coordinator inside a linked worktree still creates and
/// opens from the parent. A repository herdr names no source for is used as
/// given.
fn source_checkout(engine: &Engine, folder: &Path) -> Result<PathBuf> {
    let listing = engine.herdr.worktree_list(Some(folder)).map_err(|error| {
        at_step(
            &format!("herdr worktree.list in {}", folder.display()),
            error,
        )
    })?;
    Ok(listing
        .source_checkout
        .unwrap_or_else(|| folder.to_path_buf()))
}

/// An existing worktree gets a new tab (spec 7.2): the tab herdr returns from
/// `worktree.open` may be one the user already works in, so it is never used.
/// The open starts from `source`, the repository's source checkout, so herdr
/// searches that repository and not its focused workspace's.
fn opened(engine: &Engine, kind: &Worktree, path: &Path, source: &Path) -> Result<Placement> {
    let parent_before = engine.herdr.workspace_for_folder(source)?;
    let info = engine.herdr.worktree_open(path, source)?;
    let mut opened_workspaces = parent_opened(engine, source, parent_before)?;
    if !info.already_open {
        opened_workspaces.push(info.workspace_id.clone());
    }
    Ok(Placement {
        kind: kind.clone(),
        workspace_id: info.workspace_id,
        path: info.path,
        initial_tab: None,
        owns_initial_tab: false,
        repo: None,
        opened_workspaces,
    })
}

/// The repository's parent space, when the worktree action just opened it:
/// herdr opens that space, with one shell pane, before creating or opening
/// a worktree of a repository that has none, and its result does not say so
/// (seen live 2026-10-15: a parent space and an idle shell left behind after
/// every `path:` worker was released).
fn parent_opened(engine: &Engine, source: &Path, before: Option<String>) -> Result<Vec<String>> {
    if before.is_some() {
        return Ok(Vec::new());
    }
    Ok(engine
        .herdr
        .workspace_for_folder(source)?
        .into_iter()
        .collect())
}

/// A plain project folder herdr does not track as a worktree: the workspace
/// whose panes already sit there, else a new workspace named after the
/// folder, whose first tab becomes the worker's.
fn folder(engine: &Engine, kind: &Worktree, path: &Path) -> Result<Placement> {
    if let Some(workspace_id) = engine.herdr.workspace_for_folder(path)? {
        return Ok(Placement {
            kind: kind.clone(),
            workspace_id,
            path: path.to_path_buf(),
            initial_tab: None,
            owns_initial_tab: false,
            repo: None,
            opened_workspaces: Vec::new(),
        });
    }
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("worker");
    let created = engine.herdr.workspace_create(path, label)?;
    Ok(Placement {
        kind: kind.clone(),
        workspace_id: created.workspace_id,
        path: path.to_path_buf(),
        initial_tab: Some(created.tab),
        owns_initial_tab: true,
        repo: None,
        opened_workspaces: Vec::new(),
    })
}

fn resolve_repo(engine: &Engine, repo: Option<&str>, coordinator_path: &Path) -> Result<PathBuf> {
    if let Some(repo) = repo {
        if let Some(path) = repo.strip_prefix("path:") {
            return Ok(PathBuf::from(path));
        }
        if let Some(name) = repo.strip_prefix("name:") {
            // A configured repository whose folder name is `name`.
            let mut matches = engine
                .config
                .repos
                .keys()
                .filter(|path| path.file_name().is_some_and(|file| file == name));
            return match (matches.next(), matches.next()) {
                (Some(path), None) => Ok(path.clone()),
                (Some(_), Some(_)) => Err(PodError::usage(format!(
                    "--repo name:{name} matches more than one configured repository; use path:<path>"
                ))),
                (None, _) => Err(PodError::usage(format!(
                    "--repo name:{name} matches no [repos] entry in config.toml; use path:<path>"
                ))),
            };
        }
        return Ok(PathBuf::from(repo));
    }
    git_root(coordinator_path).ok_or_else(|| {
        PodError::usage("--repo is required when the coordinator folder is not in a Git repository")
    })
}

/// The error with the step and folder it happened in, so a placement failure
/// says which directory a git message is about.
fn at_step(step: &str, error: PodError) -> PodError {
    PodError {
        message: format!("{step}: {}", error.message),
        ..error
    }
}

fn git_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .map(Path::to_path_buf)
}
