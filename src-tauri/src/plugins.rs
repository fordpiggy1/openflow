use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub hooks: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PluginInfo {
    pub manifest: PluginManifest,
    pub enabled: bool,
    pub path: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct HookPayload {
    pub raw_text: Option<String>,
    pub formatted_text: Option<String>,
    pub provider: Option<String>,
    pub language: Option<String>,
}

pub struct PluginManager {
    plugins_dir: PathBuf,
}

impl PluginManager {
    pub fn new() -> Self {
        let plugins_dir = dirs_next::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".openflow")
            .join("plugins");

        let _ = std::fs::create_dir_all(&plugins_dir);

        Self { plugins_dir }
    }

    pub fn list_plugins(&self) -> Vec<PluginInfo> {
        let mut plugins = Vec::new();

        let entries = match std::fs::read_dir(&self.plugins_dir) {
            Ok(e) => e,
            Err(_) => return plugins,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let manifest_path = path.join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }

            let content = match std::fs::read_to_string(&manifest_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let manifest: PluginManifest = match serde_json::from_str(&content) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let enabled_path = path.join(".enabled");
            plugins.push(PluginInfo {
                manifest,
                enabled: enabled_path.exists(),
                path: path.to_string_lossy().to_string(),
            });
        }

        plugins
    }

    pub fn enable_plugin(&self, id: &str) -> Result<(), String> {
        let plugin_dir = self.plugins_dir.join(id);
        if !plugin_dir.exists() {
            return Err(format!("Plugin '{}' not found", id));
        }
        std::fs::write(plugin_dir.join(".enabled"), "")
            .map_err(|e| format!("Failed to enable: {}", e))
    }

    pub fn disable_plugin(&self, id: &str) -> Result<(), String> {
        let plugin_dir = self.plugins_dir.join(id);
        let enabled_path = plugin_dir.join(".enabled");
        if enabled_path.exists() {
            std::fs::remove_file(enabled_path)
                .map_err(|e| format!("Failed to disable: {}", e))?;
        }
        Ok(())
    }

    pub fn get_enabled_hooks(&self, hook_name: &str) -> Vec<PluginInfo> {
        self.list_plugins()
            .into_iter()
            .filter(|p| p.enabled && p.manifest.hooks.contains(&hook_name.to_string()))
            .collect()
    }

    pub fn install_plugin(&self, manifest_json: &str) -> Result<PluginInfo, String> {
        let manifest: PluginManifest = serde_json::from_str(manifest_json)
            .map_err(|e| format!("Invalid manifest: {}", e))?;

        let plugin_dir = self.plugins_dir.join(&manifest.id);
        std::fs::create_dir_all(&plugin_dir)
            .map_err(|e| format!("Failed to create plugin dir: {}", e))?;

        std::fs::write(plugin_dir.join("manifest.json"), manifest_json)
            .map_err(|e| format!("Failed to write manifest: {}", e))?;

        Ok(PluginInfo {
            manifest,
            enabled: false,
            path: plugin_dir.to_string_lossy().to_string(),
        })
    }
}

unsafe impl Send for PluginManager {}
unsafe impl Sync for PluginManager {}
