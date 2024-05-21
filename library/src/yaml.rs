use serde::Deserialize;

/// Struct for parsing shorebird.yaml.
#[derive(Deserialize)]
pub struct YamlConfig {
    /// App ID.  Required.  Generated by Shorebird and included
    /// in your app to identify which app/channel/version triple to update.
    pub app_id: String,
    /// Update channel name.  Defaults to "stable" if not set.
    pub channel: Option<String>,
    /// Update URL.  Defaults to the default update URL if not set.
    pub base_url: Option<String>,
    /// Update behavior. Defaults to true if not set.
    pub auto_update: Option<bool>,
    /// Base64-encoded public key for verifying patch hash signatures.
    pub patch_public_key: Option<String>,
}

impl YamlConfig {
    /// Read in shorebird.yaml from a string.
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }
}
