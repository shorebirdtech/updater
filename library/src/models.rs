use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct Patch {
    /// The patch number.  Starts at 1 for each new release and increases
    /// monotonically.
    pub number: usize,
    /// The hex-encoded sha256 hash of the final uncompressed patch file.
    /// Legacy: originally "#" before we implemented hash checks (remove).
    pub hash: String,
    /// The URL to download the patch file from.
    pub download_url: String,
}

#[derive(Debug, Serialize)]
pub struct PatchCheckRequest {
    /// The Shorebird app_id built into the shorebird.yaml in the app.
    pub app_id: String,
    /// The Shorebird channel built into the shorebird.yaml in the app.
    pub channel: String,
    /// The release version from AndroidManifest.xml, Info.plist in the app.
    pub release_version: String,
    /// The latest patch number that the client has downloaded.
    /// Not necessarily the one it's running (if some have been marked bad).
    /// We could rename this to be more clear.    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_number: Option<usize>,
    /// Platform (e.g. "android", "ios", "windows", "macos", "linux").
    pub platform: String,
    /// Architecture we're running (e.g. "aarch64", "x86", "x86_64").
    pub arch: String,
}

#[derive(Debug, Deserialize)]
pub struct PatchCheckResponse {
    pub patch_available: bool,
    #[serde(default)]
    pub patch: Option<Patch>,
}

#[cfg(test)]
mod tests {
    use super::{PatchCheckRequest, PatchCheckResponse};

    #[test]
    fn check_patch_request_serialization() {
        let request = PatchCheckRequest {
            app_id: "com.example.app".to_string(),
            channel: "stable".to_string(),
            release_version: "1.0.0".to_string(),
            patch_number: Some(1),
            platform: "android".to_string(),
            arch: "aarch64".to_string(),
        };

        let serialized = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serialized,
            r###"{"app_id":"com.example.app","channel":"stable","release_version":"1.0.0","patch_number":1,"platform":"android","arch":"aarch64"}"###
        )
    }

    #[test]
    fn check_patch_response_deserialization() {
        let data = r###"
    {
        "patch_available": true,
        "patch": {
            "number": 1,
            "download_url": "https://storage.googleapis.com/patch_artifacts/17a28ec1-00cf-452d-bdf9-dbb9acb78600/dlc.vmcode",
            "hash": "#"
        }
    }"###;

        let response: PatchCheckResponse = serde_json::from_str(data).unwrap();

        assert!(response.patch_available == true);
        assert!(response.patch.is_some());

        let patch = response.patch.unwrap();
        assert_eq!(patch.number, 1);
        assert_eq!(patch.download_url, "https://storage.googleapis.com/patch_artifacts/17a28ec1-00cf-452d-bdf9-dbb9acb78600/dlc.vmcode");
        assert_eq!(patch.hash, "#");
    }
}
