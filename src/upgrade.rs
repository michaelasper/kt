use anyhow::{Context, Result};
use console::style;
use dialoguer::Confirm;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;
use semver::Version;
use serde::Deserialize;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{debug, info};

const GITHUB_API: &str = "https://api.github.com/repos/michaelasper/kt/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Deserialize, Clone)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub html_url: String,
    pub assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

pub struct Upgrader {
    client: Client,
    current_version: Version,
}

#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    #[error("GitHub API error: {0}")]
    GitHubApi(String),
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Version parse error: {0}")]
    VersionParse(#[from] semver::Error),
    #[error("No suitable binary found for your platform")]
    NoBinaryFound,
    #[error("Already up to date")]
    AlreadyUpToDate,
    #[error("User cancelled")]
    Cancelled,
}

impl Upgrader {
    pub fn new() -> Result<Self> {
        let current_version =
            Version::parse(CURRENT_VERSION).context("Failed to parse current version")?;

        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("kt-updater")
            .build()?;

        Ok(Self {
            client,
            current_version,
        })
    }

    pub async fn check_updates(&self) -> Result<Option<GitHubRelease>> {
        info!("Checking for updates...");

        let release = self.fetch_latest_release().await?;
        let tag = release.tag_name.trim_start_matches('v');
        let latest_version = Version::parse(tag).context("Failed to parse latest version")?;

        if latest_version <= self.current_version {
            return Ok(None);
        }

        Ok(Some(release))
    }

    async fn fetch_latest_release(&self) -> Result<GitHubRelease> {
        let response = self
            .client
            .get(GITHUB_API)
            .header("Accept", "application/vnd.github.v3+json")
            .send()
            .await
            .context("Failed to fetch release info from GitHub")?;

        if !response.status().is_success() {
            return Err(UpgradeError::GitHubApi(format!(
                "GitHub API returned status: {}",
                response.status()
            ))
            .into());
        }

        response
            .json()
            .await
            .context("Failed to parse GitHub release")
    }

    pub async fn upgrade(&self, force: bool, target_version: Option<String>) -> Result<()> {
        println!();
        println!(
            "{}",
            style("╔════════════════════════════════════════════════════════════╗").cyan()
        );
        println!(
            "{}",
            style("║                    kt Updater                            ║").cyan()
        );
        println!(
            "{}",
            style("╚════════════════════════════════════════════════════════════╝").cyan()
        );
        println!();

        let release = if let Some(version) = target_version {
            self.fetch_release_by_version(&version).await?
        } else {
            let update_available = self.check_updates().await?;
            match update_available {
                Some(release) => release,
                None if !force => {
                    println!(
                        "{} {}",
                        style("✓").green(),
                        style(format!(
                            "kt is already up to date (v{})",
                            self.current_version
                        ))
                        .green()
                    );
                    return Ok(());
                }
                None => {
                    println!(
                        "{} {}",
                        style("✓").green(),
                        style(format!(
                            "kt is already up to date (v{}), forcing re-install...",
                            self.current_version
                        ))
                        .yellow()
                    );
                    self.fetch_latest_release().await?
                }
            }
        };

        let latest_version = Version::parse(release.tag_name.trim_start_matches('v'))?;

        println!(
            "{} New version available: {}",
            style("→").cyan(),
            style(&release.tag_name).green().bold()
        );
        println!(
            "{} Current version:    {}",
            style("→").cyan(),
            style(format!("v{}", self.current_version)).dim()
        );
        println!(
            "{} Release notes:     {}",
            style("→").cyan(),
            style(&release.html_url).blue().underlined()
        );
        println!();

        if !force {
            let confirm = Confirm::new()
                .with_prompt("Do you want to upgrade now?")
                .default(true)
                .interact()?;

            if !confirm {
                return Err(UpgradeError::Cancelled.into());
            }
        }

        let asset = self.find_suitable_asset(&release.assets)?;

        println!(
            "{} Downloading: {} ({})",
            style("→").cyan(),
            style(&asset.name).white(),
            style(format_size(asset.size)).dim()
        );

        let binary_path = self.download_binary(asset).await?;

        println!(
            "{} {}",
            style("✓").green(),
            style("Download complete").green()
        );

        self.install_binary(&binary_path)?;

        println!();
        println!(
            "{} {}",
            style("✓").green(),
            style(format!("Successfully upgraded to v{}!", latest_version))
                .green()
                .bold()
        );
        println!("  Verify: {}", style("kt --version").cyan());
        println!();

        Ok(())
    }

    async fn fetch_release_by_version(&self, version: &str) -> Result<GitHubRelease> {
        let url = format!(
            "https://api.github.com/repos/michaelasper/kt/releases/tags/{}",
            version.trim_start_matches('v')
        );

        let response = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github.v3+json")
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(UpgradeError::GitHubApi(format!(
                "Failed to fetch release {}: {}",
                version,
                response.status()
            ))
            .into());
        }

        response.json().await.map_err(Into::into)
    }

    fn find_suitable_asset<'a>(&self, assets: &'a [GitHubAsset]) -> Result<&'a GitHubAsset> {
        let (os, arch) = self.detect_platform();

        let pattern = format!("kt-{}-{}", os, arch);

        debug!("Looking for asset matching: {}", pattern);

        Ok(assets
            .iter()
            .find(|a| a.name.to_lowercase().contains(&pattern.to_lowercase()))
            .ok_or_else(|| UpgradeError::NoBinaryFound)?)
    }

    fn detect_platform(&self) -> (&'static str, &'static str) {
        let os = match std::env::consts::OS {
            "macos" => "darwin",
            "linux" => "linux",
            _ => std::env::consts::OS,
        };

        let arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            _ => std::env::consts::ARCH,
        };

        (os, arch)
    }

    async fn download_binary(&self, asset: &GitHubAsset) -> Result<PathBuf> {
        let pb = ProgressBar::new(asset.size);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .map_err(|e| anyhow::anyhow!("Invalid progress bar style: {}", e))?
            .progress_chars("#>-"));

        let response = self.client.get(&asset.browser_download_url).send().await?;

        if !response.status().is_success() {
            return Err(
                UpgradeError::GitHubApi(format!("Download failed: {}", response.status())).into(),
            );
        }

        let total_bytes = response.content_length().unwrap_or(asset.size);
        pb.set_length(total_bytes);

        let tmp_dir = std::env::temp_dir();
        let download_path = tmp_dir.join(&asset.name);

        let mut file = fs::File::create(&download_path)?;
        let mut downloaded = 0u64;
        let bytes = response.bytes().await?;

        let chunk_size = 8192;
        for chunk in bytes.chunks(chunk_size) {
            file.write_all(chunk)?;
            downloaded += chunk.len() as u64;
            pb.set_position(downloaded);
        }

        pb.finish_with_message("Downloaded");

        Ok(download_path)
    }

    fn install_binary(&self, download_path: &PathBuf) -> Result<()> {
        let current_exe =
            std::env::current_exe().context("Failed to get current executable path")?;

        debug!("Current executable: {:?}", current_exe);
        debug!("Downloaded archive: {:?}", download_path);

        let extracted_binary = if download_path.extension().is_some_and(|e| e == "gz") {
            let tmp_dir = std::env::temp_dir();
            let output = std::process::Command::new("tar")
                .arg("xzf")
                .arg(download_path)
                .arg("-C")
                .arg(&tmp_dir)
                .arg("kt")
                .output()
                .context("Failed to extract tar.gz archive")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(anyhow::anyhow!("tar extraction failed: {}", stderr));
            }

            let binary_path = tmp_dir.join("kt");
            if !binary_path.exists() {
                return Err(anyhow::anyhow!(
                    "Extracted binary not found at {:?}",
                    binary_path
                ));
            }
            binary_path
        } else {
            download_path.clone()
        };

        fs::set_permissions(&extracted_binary, fs::Permissions::from_mode(0o755))?;

        self_replace::self_replace(&extracted_binary)?;

        Ok(())
    }
}

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    format!("{:.2} {}", size, UNITS[unit_index])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500.00 B");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1572864), "1.50 MB");
    }

    #[test]
    fn test_detect_platform() {
        let upgrader = Upgrader::new().unwrap();
        let (os, arch) = upgrader.detect_platform();

        assert!(!os.is_empty());
        assert!(!arch.is_empty());
    }
}
