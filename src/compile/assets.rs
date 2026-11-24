use std::sync::Arc;

use super::ChangeSet;
use crate::{
    config::Project,
    ext::{Paint, PathExt},
    fs,
    internal_prelude::*,
    logger::GRAY,
    service::site::Site,
    signal::{Outcome, Product},
};
use camino::{Utf8Path, Utf8PathBuf};
use tokio::task::JoinHandle;

pub async fn assets(
    proj: &Arc<Project>,
    changes: &ChangeSet,
) -> JoinHandle<Result<Outcome<Product>>> {
    let changes = changes.clone();

    let proj = proj.clone();
    tokio::spawn(async move {
        if !changes.need_assets_change() {
            return Ok(Outcome::Success(Product::None));
        }
        let Some(assets) = &proj.assets else {
            return Ok(Outcome::Success(Product::None));
        };
        // if reserved.contains(assets.dir) {
        //     warn!("Assets reserved filename for Leptos. Please remove {watched:?}");
        //     return Ok(false);
        // }
        trace!("Assets starting resync");
        resync(&assets.dir, &proj.site, &proj.working_dir).await?;
        debug!("Assets finished");
        Ok(Outcome::Success(Product::Assets))
    })
}

pub fn reserved(src: &Utf8Path, pkg_dir: Option<&Utf8Path>) -> Vec<Utf8PathBuf> {
    let mut paths = vec![src.join("index.html")];
    paths.extend(pkg_dir.map(|pkg_dir| src.join(pkg_dir)));
    paths
}

// pub async fn update(config: &Config) -> Result<()> {
//     if let Some(src) = &config.leptos.assets_dir {
//         let dest = DEST.to_canoncial_dir().dot()?;
//         let src = src.to_canoncial_dir().dot()?;

//         resync(&src, &dest)
//             .await
//             .context(format!("Could not synchronize {src:?} with {dest:?}"))?;
//     }
//     Ok(())
// }

async fn resync(src: &Utf8Path, site: &Site, workspace_root: &Utf8Path) -> Result<()> {
    let dest = &site.root_dir;
    let pkg_dir = site.pkg_dir_in_root(workspace_root);
    let pkg_dir = pkg_dir.as_deref();
    let pkg_is_site_root = site.pkg_root.is_some() && pkg_dir == Some(Utf8Path::new(""));
    if !pkg_is_site_root {
        clean_dest(dest, pkg_dir)
            .await
            .wrap_err(format!("Cleaning {dest:?}"))?;
    }
    let reserved = reserved(src, pkg_dir);
    mirror(src, dest, &reserved)
        .await
        .wrap_err(format!("Mirroring {src:?} -> {dest:?}"))
}

async fn clean_dest(dest: &Utf8Path, pkg_dir: Option<&Utf8Path>) -> Result<()> {
    if !dest.exists() {
        return Ok(());
    }
    let pkg_dir = pkg_dir.map(|pkg_dir| dest.join(pkg_dir));
    let mut entries = fs::read_dir(dest).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        if entry.file_type().await?.is_dir() {
            if !pkg_dir
                .as_ref()
                .is_some_and(|pkg_dir| pkg_dir.starts_with(&path))
            {
                debug!(
                    "Assets removing folder {}",
                    GRAY.paint(path.to_string_lossy()),
                );
                fs::remove_dir_all(path).await?;
            }
        } else if entry.file_name() != "index.html" {
            debug!(
                "Assets removing file {}",
                GRAY.paint(path.to_string_lossy())
            );
            fs::remove_file(path).await?;
        }
    }
    Ok(())
}

async fn mirror(src_root: &Utf8Path, dest_root: &Utf8Path, reserved: &[Utf8PathBuf]) -> Result<()> {
    if !src_root.exists() {
        warn!(
            "Assets source {} does not exist, skipping copying assets.",
            GRAY.paint(src_root.as_str())
        );
        return Ok(());
    }

    // reserved paths should be relative to the source root
    for r in reserved {
        if r.exists() {
            return Err(eyre!(
                "Assets source {} contains path {} reserved for Leptos.\nThe build process potentially generates files at the same location in the site directory, so the asset cannot be copied to the site directory.\nPlease move, rename or remove this file or directory.",
                src_root,
                r,
            ));
        }
    }

    let mut entries = src_root.read_dir_utf8()?;
    while let Some(Ok(entry)) = entries.next() {
        let from = entry.path().to_path_buf();
        let to = from.rebase(src_root, dest_root)?;

        if entry.file_type()?.is_dir() {
            debug!(
                "Assets copy folder {} -> {}",
                GRAY.paint(from.as_str()),
                GRAY.paint(to.as_str())
            );
            fs::copy_dir_all(from, to).await?;
        } else {
            debug!(
                "Assets copy file {} -> {}",
                GRAY.paint(from.as_str()),
                GRAY.paint(to.as_str())
            );
            fs::copy(from, to).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProjectConfig;
    use temp_dir::TempDir;

    #[test]
    fn reserves_the_pkg_dir_inside_the_site_root() {
        let src = Utf8Path::new("assets");
        assert_eq!(
            reserved(src, Some(Utf8Path::new("static/pkg"))),
            ["assets/index.html", "assets/static/pkg"]
        );
        assert_eq!(reserved(src, None), ["assets/index.html"]);
        assert_eq!(
            reserved(src, Some(Utf8Path::new(""))),
            [src.join("index.html"), src.to_path_buf()]
        );
    }

    #[tokio::test]
    async fn cleans_dest_around_an_inside_or_outside_pkg_dir() {
        for pkg_dir in [Some(Utf8Path::new("static/pkg")), None] {
            let dir = TempDir::new().unwrap();
            let dest = Utf8Path::from_path(dir.path()).unwrap();
            std::fs::create_dir_all(dest.join("static/pkg")).unwrap();
            std::fs::create_dir(dest.join("images")).unwrap();
            for file in ["static/pkg/app.js", "index.html", "robots.txt"] {
                std::fs::write(dest.join(file), "").unwrap();
            }

            clean_dest(dest, pkg_dir).await.unwrap();

            assert_eq!(dest.join("static/pkg/app.js").exists(), pkg_dir.is_some());
            assert!(dest.join("index.html").exists());
            assert!(!dest.join("images").exists());
            assert!(!dest.join("robots.txt").exists());
        }
    }

    #[tokio::test]
    async fn protects_the_whole_dest_when_the_pkg_dir_is_the_site_root() {
        let dir = TempDir::new().unwrap();
        let workspace = Utf8Path::from_path(dir.path()).unwrap();
        let mut config: ProjectConfig =
            serde_json::from_str(r#"{"site-pkg-root":"./site"}"#).unwrap();
        config.site_root = workspace.join("site");
        std::fs::create_dir(&config.site_root).unwrap();
        let output = config.site_root.join("app.js");
        std::fs::write(&output, "").unwrap();
        let src = workspace.join("assets");
        std::fs::create_dir(&src).unwrap();

        let error = resync(&src, &Site::new(&config), workspace)
            .await
            .unwrap_err();

        assert!(error
            .root_cause()
            .to_string()
            .contains("reserved for Leptos"));
        assert!(output.exists());
    }
}
