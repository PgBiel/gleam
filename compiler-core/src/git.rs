use crate::io::{CommandExecutor, FileSystemReader, Stdio};
use crate::manifest::{ManifestPackage, ManifestPackageSource};
use crate::paths::ProjectPaths;
use crate::{paths, Error};
use camino::Utf8Path;
use debug_ignore::DebugIgnore;
use flate2::read::GzDecoder;
use futures::future;
use hexpm::version::Version;
use tar::Archive;

#[derive(Debug)]
pub struct Downloader {
    fs_reader: DebugIgnore<Box<dyn FileSystemReader>>,
    executor: DebugIgnore<Box<dyn CommandExecutor>>,
    paths: ProjectPaths,
}

impl Downloader {
    pub fn new(
        fs_reader: Box<dyn FileSystemReader>,
        executor: Box<dyn CommandExecutor>,
        paths: ProjectPaths,
    ) -> Self {
        Self {
            fs_reader: DebugIgnore(fs_reader),
            executor: DebugIgnore(executor),
            paths,
        }
    }

    async fn ensure_package_repository_cloned(
        &self,
        name: &str,
        url: &str,
    ) -> crate::Result<bool, Error> {
        let repository_path = self.paths.build_packages_package(name);
        if self.fs_reader.is_directory(&repository_path) {
            tracing::info!(package = name, repo = url, "git_package_in_target");
            return Ok(false);
        }

        tracing::info!(package = name, repo = url, "cloning_git_package_to_target");

        let _ = self.executor.exec(
            "git",
            &[
                "clone".into(),
                "--".into(),
                url.into(),
                repository_path.into(),
            ],
            &[],
            None,
            Stdio::Null,
        )?;

        Ok(true)
    }

    /// Clones the git package's repository to the build directory if needed.
    /// Additionally, checks it out to a specific commit, if specified.
    pub async fn ensure_git_package_in_build_directory(
        &self,
        name: &str,
        repo: &str,
        commit: Option<&str>,
    ) -> crate::Result<bool> {
        let cloned = self.ensure_package_repository_cloned(name, repo).await?;

        let checkout_done = match commit {
            Some(commit) => self.checkout_package_repository_to_commit(&name, commit)?,
            None => false,
        };

        Ok(cloned || checkout_done)
    }

    fn checkout_package_repository_to_commit(
        &self,
        name: &str,
        commit: &str,
    ) -> crate::Result<bool> {
        let repository_path = self.paths.build_packages_package(name);

        let commit_exists = self
            .executor
            .exec(
                "git",
                &[
                    "cat-file".into(),
                    "commit".into(),
                    "--".into(),
                    commit.into(),
                ],
                &[],
                Some(&repository_path),
                Stdio::Null,
            )
            .is_ok_and(|status| status == 0);

        if !commit_exists {
            // If the commit doesn't exist in the repository, it's possible we have an
            // outdated copy which isn't yet aware of the new commit, so we fetch from
            // the origin just in case.
            tracing::info!(package = name, "fetching_git_package_repository");
            let _ = self.executor.exec(
                "git",
                &["fetch".into()],
                &[],
                Some(&repository_path),
                Stdio::Null,
            )?;
        }

        tracing::info!(
            package = name,
            commit = commit,
            "checkout_of_git_package_repository"
        );

        let _ = self.executor.exec(
            "git",
            &[
                "checkout".into(),
                "--detach".into(),
                "--".into(),
                commit.into(),
            ],
            &[],
            Some(&repository_path),
            Stdio::Null,
        )?;

        Ok(true)
    }

    pub async fn download_git_packages<'a, Packages: Iterator<Item = &'a ManifestPackage>>(
        &self,
        packages: Packages,
        project_name: &str,
    ) -> crate::Result<()> {
        let futures = packages
            .filter(|package| project_name != package.name)
            .map(|package| {
                let ManifestPackageSource::Git { repo, commit } = &package.source else {
                    panic!("attempt to download non-git package through git")
                };

                self.ensure_package_in_build_directory(package, repo, Some(commit))
            });

        // Run the futures to download the packages concurrently
        let results = future::join_all(futures).await;

        // Count the number of packages downloaded while checking for errors
        for result in results {
            let _ = result?;
        }
        Ok(())
    }
}
