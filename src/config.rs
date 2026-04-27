use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const APP_DIR_NAME: &str = ".obscura-gateway";

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub root: PathBuf,
    pub config_file: PathBuf,
    pub database_file: PathBuf,
    pub artifacts_dir: PathBuf,
    pub profiles_dir: PathBuf,
    pub cookies_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub run_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let home = dirs::home_dir().context("failed to determine home directory")?;
        Ok(Self::from_root(home.join(APP_DIR_NAME)))
    }

    pub fn from_root(root: PathBuf) -> Self {
        Self {
            config_file: root.join("config.toml"),
            database_file: root.join("gateway.db"),
            artifacts_dir: root.join("artifacts"),
            profiles_dir: root.join("profiles"),
            cookies_dir: root.join("cookies"),
            logs_dir: root.join("logs"),
            bin_dir: root.join("bin"),
            run_dir: root.join("run"),
            root,
        }
    }

    pub fn ensure_all(&self) -> Result<()> {
        for dir in [
            &self.root,
            &self.artifacts_dir,
            &self.profiles_dir,
            &self.cookies_dir,
            &self.logs_dir,
            &self.bin_dir,
            &self.run_dir,
        ] {
            fs::create_dir_all(dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }
        Ok(())
    }

    pub fn ensure_writable(&self) -> Result<()> {
        self.ensure_all()?;
        let probe = self.run_dir.join(".write-check");
        fs::write(&probe, b"ok").with_context(|| format!("failed to write {}", probe.display()))?;
        fs::remove_file(&probe).ok();
        Ok(())
    }

    pub fn profile_dir(&self, profile_id: &str) -> PathBuf {
        self.profiles_dir.join(profile_id)
    }

    pub fn session_artifact_dir(&self, session_id: &str) -> PathBuf {
        self.artifacts_dir.join(session_id)
    }

    pub fn profile_json_cookie_path(&self, profile_id: &str) -> PathBuf {
        self.cookies_dir.join(format!("{profile_id}.json"))
    }

    pub fn profile_netscape_cookie_path(&self, profile_id: &str) -> PathBuf {
        self.cookies_dir.join(format!("{profile_id}.txt"))
    }

    pub fn obscura_bin_path(&self) -> PathBuf {
        self.bin_dir.join("obscura")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub server_url: String,
    pub api_key: String,
    pub listen_addr: String,
    pub obscura_bin: PathBuf,
    pub connect_ttl_secs: i64,
    pub idle_ttl_secs: i64,
    pub absolute_ttl_secs: i64,
    pub default_domain_policy: DomainPolicy,
    pub default_stealth: bool,
    pub default_proxy_policy: String,
    #[serde(default)]
    pub proxy_policies: BTreeMap<String, ProxyPolicyConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainPolicy {
    pub allowlist: Vec<String>,
    pub denylist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyPolicyConfig {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
}

impl Default for DomainPolicy {
    fn default() -> Self {
        Self {
            allowlist: Vec::new(),
            denylist: Vec::new(),
        }
    }
}

impl AppConfig {
    pub fn default_for_paths(_paths: &AppPaths) -> Self {
        Self {
            server_url: "http://127.0.0.1:18789".to_string(),
            api_key: uuid::Uuid::new_v4().simple().to_string(),
            listen_addr: "127.0.0.1:18789".to_string(),
            obscura_bin: PathBuf::from("obscura"),
            connect_ttl_secs: 300,
            idle_ttl_secs: 900,
            absolute_ttl_secs: 3600,
            default_domain_policy: DomainPolicy::default(),
            default_stealth: true,
            default_proxy_policy: "direct".to_string(),
            proxy_policies: BTreeMap::new(),
        }
    }

    pub fn load_or_create(paths: &AppPaths) -> Result<Self> {
        if paths.config_file.exists() {
            let raw = fs::read_to_string(&paths.config_file)
                .with_context(|| format!("failed to read {}", paths.config_file.display()))?;
            let mut config: Self = toml::from_str(&raw)
                .with_context(|| format!("failed to parse {}", paths.config_file.display()))?;
            if config.obscura_bin == paths.obscura_bin_path() {
                config.obscura_bin = PathBuf::from("obscura");
            }
            config.save(paths)?;
            Ok(config)
        } else {
            let config = Self::default_for_paths(paths);
            config.save(paths)?;
            Ok(config)
        }
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        let raw = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(&paths.config_file, raw)
            .with_context(|| format!("failed to write {}", paths.config_file.display()))
    }

    pub fn set_server_url(&mut self, value: String) {
        self.server_url = value;
    }

    pub fn set_api_key(&mut self, value: String) {
        self.api_key = value;
    }

    pub fn set_obscura_bin(&mut self, value: PathBuf) {
        self.obscura_bin = value;
    }

    pub fn set_default_proxy_policy(&mut self, value: String) {
        self.default_proxy_policy = value;
    }

    pub fn set_default_stealth(&mut self, value: bool) {
        self.default_stealth = value;
    }

    pub fn upsert_proxy_policy(&mut self, name: String, policy: ProxyPolicyConfig) {
        self.proxy_policies.insert(name, policy);
    }

    pub fn delete_proxy_policy(&mut self, name: &str) -> Result<()> {
        if name == self.default_proxy_policy {
            bail!("cannot delete the current default proxy policy");
        }
        self.proxy_policies.remove(name);
        Ok(())
    }

    pub fn resolve_proxy_url(&self, policy_name: &str) -> Result<Option<String>> {
        if policy_name == "direct" {
            return Ok(None);
        }
        let policy = self
            .proxy_policies
            .get(policy_name)
            .ok_or_else(|| anyhow::anyhow!("unknown proxy policy: {policy_name}"))?;
        let credentials = match (&policy.username, &policy.password) {
            (Some(user), Some(pass)) if !user.is_empty() => format!("{user}:{pass}@"),
            (Some(user), _) if !user.is_empty() => format!("{user}@"),
            _ => String::new(),
        };
        Ok(Some(format!(
            "{}://{}{}:{}",
            policy.scheme, credentials, policy.host, policy.port
        )))
    }

    pub fn validate_paths(&self, paths: &AppPaths) -> Result<()> {
        if !paths.root.starts_with(
            dirs::home_dir()
                .context("failed to determine home directory")?
                .join(APP_DIR_NAME),
        ) && paths.root.file_name().and_then(|v| v.to_str()) != Some(APP_DIR_NAME)
        {
            bail!("state root must live under ~/.obscura-gateway");
        }
        Ok(())
    }
}

pub fn rewrite_config_file<F>(paths: &AppPaths, mut f: F) -> Result<AppConfig>
where
    F: FnMut(&mut AppConfig),
{
    let mut config = AppConfig::load_or_create(paths)?;
    f(&mut config);
    config.save(paths)?;
    Ok(config)
}

#[cfg(test)]
pub fn is_under_root(root: &std::path::Path, child: &std::path::Path) -> bool {
    child.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_live_under_root() {
        let paths = AppPaths::from_root(PathBuf::from("/tmp/demo/.obscura-gateway"));
        assert!(is_under_root(&paths.root, &paths.config_file));
        assert!(is_under_root(&paths.root, &paths.database_file));
        assert!(is_under_root(&paths.root, &paths.cookies_dir));
    }

    #[test]
    fn proxy_url_renders_credentials() {
        let mut config =
            AppConfig::default_for_paths(&AppPaths::from_root(PathBuf::from("/tmp/x")));
        config.proxy_policies.insert(
            "test".into(),
            ProxyPolicyConfig {
                scheme: "socks5".into(),
                host: "proxy.example".into(),
                port: 1080,
                username: Some("user".into()),
                password: Some("pass".into()),
                country: None,
                city: None,
            },
        );
        assert_eq!(
            config.resolve_proxy_url("test").unwrap().as_deref(),
            Some("socks5://user:pass@proxy.example:1080")
        );
    }
}
