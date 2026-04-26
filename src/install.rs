use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::config::AppConfig;

pub fn resolve_obscura_path(config: &AppConfig) -> Option<PathBuf> {
    let configured = &config.obscura_bin;
    if configured.components().count() > 1 || configured.is_absolute() {
        if is_executable(configured) {
            return Some(configured.clone());
        }
        return None;
    }

    if is_executable(configured) {
        return Some(configured.clone());
    }

    env::var_os("PATH").and_then(|path_var| {
        env::split_paths(&path_var)
            .map(|segment| segment.join(configured))
            .find(|candidate| is_executable(candidate))
    })
}

pub fn require_obscura(config: &AppConfig) -> Result<PathBuf> {
    resolve_obscura_path(config).ok_or_else(|| {
        anyhow!(
            "obscura binary not found for configured path `{}`. Install obscura separately or set `obscura_bin` in config.",
            config.obscura_bin.display()
        )
    })
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, AppPaths};

    #[test]
    fn missing_binary_is_not_resolved() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().join(".obscura-gateway"));
        let mut config = AppConfig::default_for_paths(&paths);
        config.obscura_bin = PathBuf::from("definitely-missing-obscura");
        assert!(resolve_obscura_path(&config).is_none());
    }
}
