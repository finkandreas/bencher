use bencher_client::{
    types::{JsonNewBranch, JsonNewStartPoint, JsonUpdateBranch},
    ClientError, ErrorResponse,
};
use bencher_json::{
    project::branch::BRANCH_MAIN_STR, BranchName, GitHash, JsonBranch, JsonBranches,
    JsonStartPoint, NameId, NameIdKind, ResourceId, Slug,
};

use crate::{
    bencher::backend::AuthBackend, cli_println_quietable, parser::project::run::CliRunBranch,
};

use super::BENCHER_BRANCH;

#[derive(Debug, Clone)]
pub struct Branch {
    branch: NameId,
    start_point: Option<StartPoint>,
}

#[derive(Debug, Clone)]
pub struct StartPoint {
    branch: NameId,
    hash: Option<GitHash>,
    self_ref: bool,
}

impl StartPoint {
    // Check to see if the start point branch is the same as the specified branch.
    fn is_self_ref(&self) -> bool {
        self.self_ref
    }

    // Check to see if the start point branch matches the specified branch.
    // If so, then the branch will start from the renamed version of itself.
    fn rename_self_ref(mut self, branch: &JsonBranch) -> Self {
        if self.is_self_ref() {
            self.branch = branch.uuid.into();
            self
        } else {
            self
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum BranchError {
    #[error("Failed to parse UUID, slug, or name for the branch: {0}")]
    ParseBranch(bencher_json::ValidError),
    #[error("Failed to get branch with UUID: {0}\nDoes it exist? Branches must already exist when using `--branch` or `BENCHER_BRANCH` with a UUID.\nSee: https://bencher.dev/docs/explanation/branch-selection/")]
    GetBranchUuid(crate::BackendError),
    #[error("Failed to get branch with slug: {0}")]
    GetBranchSlug(crate::BackendError),
    #[error("Failed to query branches: {0}")]
    GetBranches(crate::BackendError),
    #[error(
        "{count} branches were found with name \"{branch_name}\" in project \"{project}\"! Exactly one was expected.\nThis is likely a bug. Please report it here: https://github.com/bencherdev/bencher/issues"
    )]
    MultipleBranches {
        project: String,
        branch_name: String,
        count: usize,
    },
    #[error("Failed to get branch start point: {0}\nDoes it exist? The branch start point must already exist when using `--branch-start-point`\nSee: https://bencher.dev/docs/explanation/branch-selection/")]
    GetStartPoint(crate::BackendError),
    #[error(
        "No branches were found for the start point with name \"{branch_name}\" in project \"{project}\". Exactly one was expected.\nDoes it exist?"
    )]
    NoStartPoint {
        project: String,
        branch_name: String,
    },
    #[error("Failed to get current branch start point. This is likely a bug. Please report it here: https://github.com/bencherdev/bencher/issues")]
    GetCurrentStartPoint(crate::BackendError),
    #[error("Failed to create new branch: {0}")]
    CreateBranch(crate::BackendError),
    #[error("Failed to update detached branch: {0}")]
    UpdateBranch(crate::BackendError),
}

impl TryFrom<CliRunBranch> for Branch {
    type Error = BranchError;

    fn try_from(run_branch: CliRunBranch) -> Result<Self, Self::Error> {
        let CliRunBranch {
            branch,
            branch_start_point,
            branch_start_point_hash,
            deprecated: _,
        } = run_branch;
        let branch = if let Some(branch) = branch {
            branch
        } else if let Ok(env_branch) = std::env::var(BENCHER_BRANCH) {
            env_branch
                .as_str()
                .parse()
                .map_err(BranchError::ParseBranch)?
        } else {
            BRANCH_MAIN_STR.parse().map_err(BranchError::ParseBranch)?
        };
        let start_point = branch_start_point.first().cloned().and_then(|b| {
            // The only invalid `NameId` is an empty string.
            // This allows for "continue on empty" semantics for the branch start point.
            let start_point_branch = b.parse().ok()?;
            let self_ref = branch == start_point_branch;
            Some(StartPoint {
                branch: start_point_branch,
                hash: branch_start_point_hash,
                self_ref,
            })
        });
        Ok(Self {
            branch,
            start_point,
        })
    }
}

impl Branch {
    pub async fn get(
        &self,
        project: &ResourceId,
        dry_run: bool,
        log: bool,
        backend: &AuthBackend,
    ) -> Result<NameId, BranchError> {
        if !dry_run {
            self.exists_or_create(project, log, backend).await?;
        }
        Ok(self.branch.clone())
    }

    async fn exists_or_create(
        &self,
        project: &ResourceId,
        log: bool,
        backend: &AuthBackend,
    ) -> Result<(), BranchError> {
        // Check to make sure that the branch exists before running the benchmarks.
        // Then check that the start point branch matches, if specified.
        match (&self.branch)
            .try_into()
            .map_err(BranchError::ParseBranch)?
        {
            NameIdKind::Uuid(uuid) => {
                let branch = get_branch(project, &uuid.into(), backend)
                    .await
                    .map_err(BranchError::GetBranchUuid)?;
                self.check_start_point(project, branch, log, backend)
                    .await?;
            },
            NameIdKind::Slug(slug) => {
                match get_branch(project, &slug.clone().into(), backend).await {
                    Ok(branch) => {
                        self.check_start_point(project, branch, log, backend)
                            .await?;
                    },
                    Err(crate::BackendError::Client(ClientError::ErrorResponse(
                        ErrorResponse {
                            status: reqwest::StatusCode::NOT_FOUND,
                            ..
                        },
                    ))) => {
                        cli_println_quietable!(
                            log,
                            "Failed to find branch with slug \"{slug}\" in project \"{project}\"."
                        );
                        create_branch(
                            project,
                            slug.clone().into(),
                            Some(slug),
                            self.start_point.clone(),
                            log,
                            backend,
                        )
                        .await?;
                    },
                    Err(e) => return Err(BranchError::GetBranchSlug(e)),
                }
            },
            NameIdKind::Name(name) => match get_branch_by_name(project, &name, backend).await {
                Ok(Some(branch)) => {
                    self.check_start_point(project, branch, log, backend)
                        .await?;
                },
                Ok(None) => {
                    cli_println_quietable!(
                        log,
                        "Failed to find branch with name \"{name}\" in project \"{project}\"."
                    );
                    create_branch(project, name, None, self.start_point.clone(), log, backend)
                        .await?;
                },
                Err(e) => return Err(e),
            },
        }
        Ok(())
    }

    async fn check_start_point(
        &self,
        project: &ResourceId,
        current_branch: JsonBranch,
        log: bool,
        backend: &AuthBackend,
    ) -> Result<(), BranchError> {
        // Compare the current start point against the provided start point.
        match (current_branch.start_point.clone(), self.start_point.clone()) {
            // If there is both a current and provided start point, then they need to be compared.
            (Some(current_start_point), Some(start_point)) => {
                // Get the branch for the provided start point.
                let start_point_branch = match (&start_point.branch)
                    .try_into()
                    .map_err(BranchError::ParseBranch)?
                {
                    NameIdKind::Uuid(uuid) => get_branch(project, &uuid.into(), backend)
                        .await
                        .map_err(BranchError::GetStartPoint),
                    NameIdKind::Slug(slug) => get_branch(project, &slug.into(), backend)
                        .await
                        .map_err(BranchError::GetStartPoint),
                    NameIdKind::Name(name) => get_branch_by_name(project, &name, backend)
                        .await?
                        .ok_or_else(|| BranchError::NoStartPoint {
                            project: project.to_string(),
                            branch_name: name.as_ref().into(),
                        }),
                }?;

                // If the current start point branch does not match the provided start point branch, then the branch needs to be recreated from that new start point.
                if current_start_point.branch != start_point_branch.uuid {
                    cli_println_quietable!(
                        log,
                        "Current start point branch ({current}) is different than the specified start point branch ({specified}).",
                        current = current_start_point.branch,
                        specified = start_point_branch.uuid,
                    );
                    return rename_and_create_branch(
                        project,
                        &current_branch,
                        start_point,
                        log,
                        backend,
                    )
                    .await;
                }

                // If both the current and provided start point branches match, then check the hashes.
                match (&current_start_point.version.hash, &start_point.hash) {
                    (Some(current_hash), Some(hash)) => {
                        // Rename and create a new branch if the hashes do not match.
                        if current_hash != hash {
                            cli_println_quietable!(
                                log,
                                "Current start point branch hash ({current_hash}) is different than the specified start point branch hash ({hash}).",
                            );
                            rename_and_create_branch(
                                project,
                                &current_branch,
                                start_point,
                                log,
                                backend,
                            )
                            .await?;
                        }
                    },
                    // Rename the current branch if it does not have a start point hash and the provided start point does.
                    // This should only rarely happen going forward, as most branches with a start point will have a hash.
                    (None, Some(_)) => {
                        cli_println_quietable!(
                            log,
                            "No current start point branch hash and a start point branch hash was specified.",
                        );
                        rename_and_create_branch(
                            project,
                            &current_branch,
                            start_point,
                            log,
                            backend,
                        )
                        .await?;
                    },
                    // If a start point hash is not specified, then there is nothing to check.
                    // Even if the current branch has a start point hash, it does not need to always be specified.
                    // That is, adding a start point hash is a one way operation with `bencher run`.
                    // Alternatively, this could actually follow the HEAD here, so not specifying a hash is equivalent to specifying the HEAD.
                    // However, that behavior will likely be confusing to users.
                    // Further, this would be a breaking change for users who have already specified a start point without a hash.
                    (_, None) => {},
                }
            },
            // If the current branch does not have a start point and one is specified, then the branch needs to be recreated from that start point.
            // Because adding a start point is a one way operation with `bencher run`, this operation will only ever be performed once.
            // Therefore, using a set naming convention for the detached branch name and slug is okay: `branch_name@detached`
            (None, Some(start_point)) => {
                cli_println_quietable!(
                    log,
                    "No current start point branch and a start point branch was specified.",
                );
                rename_and_create_branch(project, &current_branch, start_point, log, backend)
                    .await?;
            },
            // If a start point is not specified, then there is nothing to check.
            // Even if the current branch has a start point, it does not need to always be specified.
            // That is, adding a start point is a one way operation with `bencher run`.
            // Alternatively, this could actually rename and create a new branch if there is a current start point,
            // so not specifying a start point when there is a current start point is equivalent to resetting the branch.
            // However, that behavior will likely be confusing to users.
            (_, None) => {},
        }

        Ok(())
    }
}

async fn get_branch(
    project: &ResourceId,
    branch: &ResourceId,
    backend: &AuthBackend,
) -> Result<JsonBranch, crate::BackendError> {
    backend
        .send_with(|client| async move {
            client
                .proj_branch_get()
                .project(project.clone())
                .branch(branch.clone())
                .send()
                .await
        })
        .await
}

async fn get_branch_by_name(
    project: &ResourceId,
    branch_name: &BranchName,
    backend: &AuthBackend,
) -> Result<Option<JsonBranch>, BranchError> {
    let json_branches: JsonBranches = backend
        .send_with(|client| async move {
            client
                .proj_branches_get()
                .project(project.clone())
                .name(branch_name.clone())
                .send()
                .await
        })
        .await
        .map_err(BranchError::GetBranches)?;

    let mut json_branches = json_branches.into_inner();
    let branch_count = json_branches.len();
    if let Some(branch) = json_branches.pop() {
        if branch_count == 1 {
            Ok(Some(branch))
        } else {
            Err(BranchError::MultipleBranches {
                project: project.to_string(),
                branch_name: branch_name.as_ref().into(),
                count: branch_count,
            })
        }
    } else {
        Ok(None)
    }
}

async fn create_branch(
    project: &ResourceId,
    branch_name: BranchName,
    branch_slug: Option<Slug>,
    start_point: Option<StartPoint>,
    log: bool,
    backend: &AuthBackend,
) -> Result<JsonBranch, BranchError> {
    let (start_point, message) = if let Some(StartPoint { branch, hash, .. }) = start_point {
        let message = format!(
            " with start point branch \"{branch}\"{}",
            hash.as_ref()
                .map(|hash| format!(" and hash \"{hash}\""))
                .unwrap_or_default(),
        );
        // Only create a new branch with a start point if the branch is not the same as the start point.
        // This is useful for relative benchmarking, where you want to be able to create a new bare branch, without a start point,
        // and still take advantage of all the rename semantics found here.
        // Default to cloning the thresholds from the start point branch
        let start_point = JsonNewStartPoint {
            branch: branch.clone().into(),
            hash: hash.clone().map(Into::into),
            thresholds: Some(true),
        };
        (Some(start_point), Some(message))
    } else {
        (None, None)
    };
    cli_println_quietable!(
        log,
        "Creating a new branch with name \"{branch_name}\" in project \"{project}\"{message}.",
        message = message.unwrap_or_default()
    );
    let new_branch = &JsonNewBranch {
        name: branch_name.into(),
        slug: branch_slug.map(Into::into),
        soft: Some(true),
        start_point,
    };

    backend
        .send_with(|client| async move {
            client
                .proj_branch_post()
                .project(project.clone())
                .body(new_branch.clone())
                .send()
                .await
        })
        .await
        .map_err(BranchError::CreateBranch)
}

async fn rename_and_create_branch(
    project: &ResourceId,
    current_branch: &JsonBranch,
    start_point: StartPoint,
    log: bool,
    backend: &AuthBackend,
) -> Result<(), BranchError> {
    // Update the current branch name and slug
    let renamed_branch = rename_branch(project, current_branch, &start_point, log, backend).await?;
    // Update the start point branch if using a self-referential start point
    let start_point = start_point.rename_self_ref(&renamed_branch);
    // Create new branch with the same name and slug as the current branch
    create_branch(
        project,
        current_branch.name.clone(),
        Some(current_branch.slug.clone()),
        Some(start_point),
        log,
        backend,
    )
    .await?;
    Ok(())
}

async fn rename_branch(
    project: &ResourceId,
    current_branch: &JsonBranch,
    start_point: &StartPoint,
    log: bool,
    backend: &AuthBackend,
) -> Result<JsonBranch, BranchError> {
    cli_println_quietable!(
        log,
        "New start point for branch with name \"{branch_name}\" in project \"{project}\".",
        branch_name = current_branch.name.as_ref(),
    );

    let suffix = rename_branch_suffix(
        project,
        current_branch.start_point.as_ref(),
        start_point,
        backend,
    )
    .await?;
    let branch_name = format!(
        "{branch_name}@{suffix}",
        branch_name = current_branch.name.as_ref()
    );
    let branch_slug = Slug::new(&branch_name);
    cli_println_quietable!(
        log,
        "Renaming detached branch to have name \"{branch_name}\" and slug \"{branch_slug}\" in project \"{project}\"."
    );

    // TODO archive the detached branch
    match update_branch(
        project,
        &current_branch.slug.clone().into(),
        Some(branch_name.clone().into()),
        Some(branch_slug.clone().into()),
        backend,
    )
    .await
    {
        Ok(branch) => Ok(branch),
        Err(BranchError::UpdateBranch(crate::BackendError::Client(
            ClientError::ErrorResponse(ErrorResponse {
                status: reqwest::StatusCode::CONFLICT,
                ..
            }),
        ))) => {
            cli_println_quietable!(
                log,
                "Branch with name \"{branch_name}\" or slug \"{branch_slug}\" in project \"{project}\" already exists."
            );
            let branch_name = format!("{branch_name}/{random}", random = Slug::rand_suffix());
            let branch_slug = Slug::new(&branch_name);
            cli_println_quietable!(
                log,
                "Renaming detached branch to have name \"{branch_name}\" and slug \"{branch_slug}\" in project \"{project}\" to avoid conflict."
            );
            update_branch(
                project,
                &current_branch.slug.clone().into(),
                Some(branch_name.clone().into()),
                Some(branch_slug.clone().into()),
                backend,
            )
            .await
        },
        Err(e) => Err(e),
    }
}

async fn update_branch(
    project: &ResourceId,
    resource_id: &ResourceId,
    name: Option<bencher_client::types::BranchName>,
    slug: Option<bencher_client::types::Slug>,
    backend: &AuthBackend,
) -> Result<JsonBranch, BranchError> {
    let update_branch = &JsonUpdateBranch { name, slug };
    backend
        .send_with(|client| async move {
            client
                .proj_branch_patch()
                .project(project.clone())
                .branch(resource_id.clone())
                .body(update_branch.clone())
                .send()
                .await
        })
        .await
        .map_err(BranchError::UpdateBranch)
}

async fn rename_branch_suffix(
    project: &ResourceId,
    current_start_point: Option<&JsonStartPoint>,
    start_point: &StartPoint,
    backend: &AuthBackend,
) -> Result<String, BranchError> {
    let Some(current_start_point) = current_start_point else {
        return Ok("detached".to_owned());
    };

    // Get the current start point branch, with its UUID.
    let current_start_point_branch =
        get_branch(project, &current_start_point.branch.into(), backend)
            .await
            .map_err(BranchError::GetCurrentStartPoint)?;

    let branch_name = if start_point.is_self_ref() {
        "HEAD"
    } else {
        current_start_point_branch.name.as_ref()
    };
    let version_suffix = if let Some(hash) = &current_start_point.version.hash {
        format!("hash/{hash}")
    } else {
        format!("version/{}", current_start_point.version.number)
    };
    Ok(format!("{branch_name}/{version_suffix}"))
}
