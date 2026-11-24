use std::{
    collections::HashMap,
    fmt::{self, Display},
    net::SocketAddr,
};

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use tokio::sync::RwLock;

use crate::internal_prelude::*;
use crate::{
    config::ProjectConfig,
    ext::{fs, PathBufExt},
};

#[derive(Clone)]
pub struct SourcedSiteFile {
    /// source file's relative path from the root (workspace or project) directory
    pub source: Utf8PathBuf,
    /// dest file's relative path from the root (workspace or project) directory
    pub dest: Utf8PathBuf,
    /// dest file's relative path from the site directory
    pub site: Utf8PathBuf,
}

impl SourcedSiteFile {
    pub fn as_site_file(&self) -> SiteFile {
        SiteFile {
            dest: self.dest.clone(),
            site: self.site.clone(),
        }
    }
}

impl std::fmt::Debug for SourcedSiteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourcedSiteFile")
            .field("source", &self.source.test_string())
            .field("dest", &self.dest.test_string())
            .field("site", &self.site.test_string())
            .finish()
    }
}

impl Display for SourcedSiteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} -> @{}", self.source, self.site)
    }
}

#[derive(Clone)]
pub struct SiteFile {
    /// dest file's relative path from the root (workspace or project) directory
    pub dest: Utf8PathBuf,
    /// dest file's relative path from the site directory
    pub site: Utf8PathBuf,
}

impl Display for SiteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}", self.site)
    }
}

impl std::fmt::Debug for SiteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteFile")
            .field("dest", &self.dest.test_string())
            .field("site", &self.site.test_string())
            .finish()
    }
}

pub struct Site {
    pub addr: SocketAddr,
    pub reload: SocketAddr,
    pub root_dir: Utf8PathBuf,
    pub pkg_dir: Utf8PathBuf,
    pub pkg_url: Option<Utf8PathBuf>,
    pub(crate) pkg_root: Option<Utf8PathBuf>,
    file_reg: RwLock<HashMap<String, u64>>,
    ext_file_reg: RwLock<HashMap<String, u64>>,
}

impl fmt::Debug for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Site")
            .field("addr", &self.addr)
            .field("reload", &self.reload)
            .field("root_dir", &self.root_dir)
            .field("pkg_dir", &self.pkg_dir)
            .field("pkg_url", &self.pkg_url)
            .field("pkg_root", &self.pkg_root)
            .field("file_reg", &self.file_reg.blocking_read())
            .field("ext_file_reg", &self.ext_file_reg.blocking_read())
            .finish()
    }
}

impl Site {
    pub fn new(config: &ProjectConfig) -> Self {
        let mut reload = config.site_addr;
        reload.set_port(config.reload_port);
        Self {
            addr: config.site_addr,
            reload,
            root_dir: config.site_root.clone(),
            pkg_dir: config.site_pkg_dir.clone(),
            pkg_url: config.site_pkg_url.clone(),
            pkg_root: config.site_pkg_root.clone(),
            file_reg: Default::default(),
            ext_file_reg: Default::default(),
        }
    }

    pub fn root_relative_pkg_dir(&self) -> Utf8PathBuf {
        self.pkg_root
            .clone()
            .unwrap_or_else(|| self.root_dir.join(&self.pkg_dir))
    }

    /// Returns the pkg dir relative to the site root if it is inside the site root.
    pub(crate) fn pkg_dir_in_root(&self, workspace_root: &Utf8Path) -> Option<Utf8PathBuf> {
        let Some(pkg_root) = &self.pkg_root else {
            return Some(self.pkg_dir.clone());
        };
        let root = normalize(&workspace_root.join(&self.root_dir));
        let pkg = normalize(&workspace_root.join(pkg_root));
        pkg.strip_prefix(&root).ok().map(Utf8Path::to_path_buf)
    }

    /// Returns the configured pkg root if it is outside the site root.
    pub(crate) fn pkg_root_outside(&self, workspace_root: &Utf8Path) -> Option<&Utf8Path> {
        self.pkg_root
            .as_deref()
            .filter(|_| self.pkg_dir_in_root(workspace_root).is_none())
    }

    /// check if the file changed
    pub async fn did_external_file_change(&self, to: &Utf8Path) -> Result<bool> {
        let new_hash = file_hash(to).await.dot()?;
        let cur_hash = { self.ext_file_reg.read().await.get(to.as_str()).copied() };
        if Some(new_hash) == cur_hash {
            return Ok(false);
        }
        let mut f = self.ext_file_reg.write().await;
        f.insert(to.to_string(), new_hash);
        trace!("Site update hash for {to} to {new_hash}");
        Ok(true)
    }

    pub async fn updated(&self, file: &SourcedSiteFile) -> Result<bool> {
        fs::create_dir_all(file.dest.clone().without_last()).await?;

        let new_hash = file_hash(&file.source).await?;
        let cur_hash = self.current_hash(&file.site, &file.dest).await?;

        if Some(new_hash) == cur_hash {
            return Ok(false);
        }
        fs::copy(&file.source, &file.dest).await?;

        let mut reg = self.file_reg.write().await;
        reg.insert(file.site.to_string(), new_hash);
        Ok(true)
    }

    /// check after writing the file if it changed
    pub async fn did_file_change(&self, file: &SiteFile) -> Result<bool> {
        let new_hash = file_hash(&file.dest).await.dot()?;
        let cur_hash = { self.file_reg.read().await.get(file.site.as_str()).copied() };
        if Some(new_hash) == cur_hash {
            return Ok(false);
        }
        let mut f = self.file_reg.write().await;
        f.insert(file.site.to_string(), new_hash);
        Ok(true)
    }

    pub async fn updated_with(&self, file: &SiteFile, data: &[u8]) -> Result<bool> {
        fs::create_dir_all(file.dest.clone().without_last()).await?;

        let new_hash = seahash::hash(data);
        let cur_hash = self.current_hash(&file.site, &file.dest).await?;

        if Some(new_hash) == cur_hash {
            return Ok(false);
        }

        fs::write(&file.dest, &data).await?;

        let mut reg = self.file_reg.write().await;
        reg.insert(file.site.to_string(), new_hash);
        Ok(true)
    }

    async fn current_hash(&self, site: &Utf8Path, dest: &Utf8Path) -> Result<Option<u64>> {
        if let Some(hash) = self.file_reg.read().await.get(site.as_str()).copied() {
            Ok(Some(hash))
        } else if dest.exists() {
            Ok(Some(file_hash(dest).await?))
        } else {
            Ok(None)
        }
    }
}

async fn file_hash(file: &Utf8Path) -> Result<u64> {
    let data = fs::read(&file).await?;
    Ok(seahash::hash(&data))
}

/// Removes `.` and resolves `..` without reading the disk.
pub(crate) fn normalize(path: &Utf8Path) -> Utf8PathBuf {
    let mut normalized = Utf8PathBuf::new();
    for component in path.components() {
        match component {
            Utf8Component::CurDir => {}
            Utf8Component::ParentDir
                if matches!(
                    normalized.components().next_back(),
                    Some(Utf8Component::Normal(_))
                ) =>
            {
                normalized.pop();
            }
            Utf8Component::ParentDir if normalized.has_root() => {}
            component => normalized.push(component.as_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_pkg_dir_relative_to_the_site_root() {
        let workspace = Utf8Path::new("/workspace");
        let mut config: ProjectConfig = serde_json::from_str("{}").unwrap();
        for (root, pkg, expected) in [
            ("target/site", "target/site/pkg", Some("pkg")),
            ("target/site", "target/site", Some("")),
            ("target/site", "target/site-pkg", None),
            (
                "./target/site",
                "./target/site/nested/pkg",
                Some("nested/pkg"),
            ),
            ("target/tmp/../site", "target/site/sub/../pkg", Some("pkg")),
            ("target/site", "target/site/../pkg", None),
            ("/workspace/target/site", "target/site/pkg", Some("pkg")),
            ("target/site", "/workspace/target/site/pkg", Some("pkg")),
        ] {
            config.site_root = root.into();
            config.site_pkg_root = Some(pkg.into());
            let site = Site::new(&config);
            assert_eq!(
                site.pkg_dir_in_root(workspace).as_deref(),
                expected.map(Utf8Path::new),
                "{root}, {pkg}",
            );
            assert_eq!(
                site.pkg_root_outside(workspace),
                expected.is_none().then_some(Utf8Path::new(pkg)),
            );
        }
    }

    #[test]
    fn retains_the_legacy_pkg_dir_without_a_pkg_root() {
        let mut config: ProjectConfig = serde_json::from_str("{}").unwrap();
        config.site_pkg_dir = "/srv/pkg".into();
        let site = Site::new(&config);
        let workspace = Utf8Path::new("/workspace");
        assert_eq!(site.pkg_dir_in_root(workspace), Some("/srv/pkg".into()));
        assert_eq!(site.pkg_root_outside(workspace), None);
    }
}
