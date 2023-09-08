// This file's job is to deal with the update_server and network side
// of the updater library.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq)]
pub enum EventType {
    PatchInstallSuccess,
    PatchInstallFailure,
}

impl Serialize for EventType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            EventType::PatchInstallSuccess => "__patch_install__",
            EventType::PatchInstallFailure => "__patch_install_failure__",
        })
    }
}

impl<'de> Deserialize<'de> for EventType {
    fn deserialize<D>(deserializer: D) -> Result<EventType, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "__patch_install__" => Ok(EventType::PatchInstallSuccess),
            "__patch_install_failure__" => Ok(EventType::PatchInstallFailure),
            _ => Err(serde::de::Error::custom(format!(
                "Unknown event type: {}",
                s
            ))),
        }
    }
}
/// Any edits to this struct should be made carefully and in accordance
/// with our privacy policy:
/// https://docs.shorebird.dev/privacy
/// An event that is sent to the server when a patch is successfully installed.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PatchEvent {
    /// The Shorebird app_id built into the shorebird.yaml in the app.
    pub app_id: String,

    /// The architecture we're running (e.g. "aarch64", "x86", "x86_64").
    pub arch: String,

    /// The unique ID of this device.
    pub client_id: String,

    /// The identifier of this event.
    #[serde(rename = "type")]
    pub identifier: EventType,

    /// The patch number that was installed.
    pub patch_number: usize,

    /// The platform we're running on (e.g. "android", "ios", "windows", "macos", "linux").
    pub platform: String,

    /// The release version from AndroidManifest.xml, Info.plist in the app.
    pub release_version: String,
}
