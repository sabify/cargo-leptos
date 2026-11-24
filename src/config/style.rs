use super::{ProjectConfig, TailwindConfig};
use crate::{
    ext::eyre::reexports::Result,
    service::site::{SiteFile, SourcedSiteFile},
};

#[derive(Debug, Clone)]
pub struct StyleConfig {
    pub file: Option<SourcedSiteFile>,
    /// whether to bundle CSS files using Lightning CSS
    pub bundle: bool,
    pub browserquery: String,
    pub tailwind: Option<TailwindConfig>,
    pub site_file: SiteFile,
}

impl StyleConfig {
    pub fn new(config: &ProjectConfig) -> Result<Self> {
        let site_file = config.pkg_file(&config.output_name, "css");
        let style_file = config.style_file.as_ref().map(|file| {
            // relative to the configuration file
            SourcedSiteFile {
                source: config.config_dir.join(file),
                dest: site_file.dest.clone(),
                site: site_file.site.clone(),
            }
        });
        Ok(Self {
            file: style_file,
            bundle: config.style_bundle,
            browserquery: config.browserquery.clone(),
            tailwind: TailwindConfig::new(config)?,
            site_file,
        })
    }
}
