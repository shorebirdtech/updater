//! Patch asset manifest parsing, validation, and path layout helpers.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::updater::UpdateError;

pub const SUPPORTED_VERSION: u32 = 1;

/// Framework-owned `flutter_assets` keys that patches must never override so
/// runtime asset and font registries stay governed by the release bundle.
pub const POISON_ASSET_KEYS: [&str; 5] = [
    "AssetManifest.json",
    "AssetManifest.bin",
    "AssetManifest.bin.json",
    "FontManifest.json",
    "NOTICES.Z",
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchAssetEncoding {
    #[default]
    Whole,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PatchAssetEntry {
    #[serde(alias = "sha256")]
    pub hash: String,
    pub size: u64,
    #[serde(default)]
    pub encoding: PatchAssetEncoding,
}

impl PatchAssetEntry {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let entry: Self = serde_json::from_slice(bytes)
            .context(UpdateError::CorruptPatch)
            .context("Malformed patch asset entry JSON")?;
        entry.validate()?;
        Ok(entry)
    }

    pub fn validate(&self) -> Result<()> {
        if !is_valid_sha256_hex(&self.hash) {
            return Err(UpdateError::CorruptPatch)
                .context(format!("Invalid patch asset SHA-256 hex {:?}", self.hash));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PatchAssetManifest {
    pub version: u32,
    pub assets: BTreeMap<String, PatchAssetEntry>,
}

impl PatchAssetManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let manifest: Self = serde_json::from_slice(bytes)
            .context(UpdateError::CorruptPatch)
            .context("Malformed patch asset manifest JSON")?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != SUPPORTED_VERSION {
            return Err(UpdateError::CorruptPatch).context(format!(
                "Unsupported patch asset manifest version {} (expected {})",
                self.version, SUPPORTED_VERSION
            ));
        }
        for (key, entry) in &self.assets {
            validate_asset_key(key)?;
            entry
                .validate()
                .with_context(|| format!("Invalid patch asset entry for {key:?}"))?;
        }
        Ok(())
    }
}

pub fn is_poison_asset_key(key: &str) -> bool {
    POISON_ASSET_KEYS.contains(&key)
}

pub fn is_valid_sha256_hex(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn validate_asset_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.starts_with('/')
        || key.ends_with('/')
        || key.contains('\\')
        || key.contains('\0')
    {
        return Err(UpdateError::CorruptPatch).context(format!(
            "Refusing patch asset path {key:?}: not a canonical relative path"
        ));
    }
    for segment in key.split('/') {
        let has_drive = segment.len() >= 2
            && segment.as_bytes()[0].is_ascii_alphabetic()
            && segment.as_bytes()[1] == b':';
        let mut comps = Path::new(segment).components();
        let single_normal = matches!(
            (comps.next(), comps.next()),
            (Some(Component::Normal(_)), None)
        );
        if segment.is_empty() || segment == "." || segment == ".." || has_drive || !single_normal {
            return Err(UpdateError::CorruptPatch).context(format!(
                "Refusing patch asset path {key:?}: not a canonical relative path"
            ));
        }
    }
    if is_poison_asset_key(key) {
        return Err(UpdateError::CorruptPatch)
            .context(format!("Refusing poisoned patch asset key {key:?}"));
    }
    Ok(())
}

pub fn blob_url(blob_base_url: &str, hash: &str) -> String {
    format!("{}/{hash}", blob_base_url.trim_end_matches('/'))
}

pub fn manifest_path(assets_dir: &Path) -> PathBuf {
    assets_dir.join("manifest.json")
}

pub fn asset_files_path(assets_dir: &Path) -> PathBuf {
    assets_dir.join("files")
}

/// Separate `files/` subdirectory keeps `flutter_assets/manifest.json` out of
/// the same namespace as our top-level `manifest.json`.
pub fn asset_path_for_key(files_root: &Path, key: &str) -> Result<PathBuf> {
    validate_asset_key(key)?;
    let mut path = files_root.to_path_buf();
    for segment in key.split('/') {
        path.push(segment);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_HASH: &str = "68e656b251e67e8358bef8483ab0d51c6619f3e7a1a9f0e75838d41ff368f728";

    fn manifest_json_with(key: &str, hash: &str) -> String {
        format!(
            r#"{{"version":1,"assets":{{"{key}":{{"hash":"{hash}","size":13,"encoding":"whole"}}}}}}"#
        )
    }

    fn assert_corrupt(err: anyhow::Error) {
        assert_eq!(
            err.downcast_ref::<UpdateError>(),
            Some(&UpdateError::CorruptPatch)
        );
    }

    #[test]
    fn parses_valid_manifest_and_entry() {
        let json = manifest_json_with("assets/images/logo.png", VALID_HASH);
        let manifest = PatchAssetManifest::parse(json.as_bytes()).unwrap();
        assert_eq!(manifest.version, SUPPORTED_VERSION);
        assert_eq!(
            manifest.assets.get("assets/images/logo.png"),
            Some(&PatchAssetEntry {
                hash: VALID_HASH.to_string(),
                size: 13,
                encoding: PatchAssetEncoding::Whole,
            })
        );
        let entry =
            PatchAssetEntry::parse(format!(r#"{{"hash":"{VALID_HASH}","size":42}}"#).as_bytes())
                .unwrap();
        assert_eq!(entry.size, 42);
        assert_eq!(entry.encoding, PatchAssetEncoding::Whole);
    }

    #[test]
    fn refuses_unsupported_version_and_unknown_encoding() {
        let v2 = manifest_json_with("assets/logo.png", VALID_HASH).replace(":1,", ":2,");
        assert_corrupt(PatchAssetManifest::parse(v2.as_bytes()).unwrap_err());
        let bad_enc =
            manifest_json_with("assets/logo.png", VALID_HASH).replace("\"whole\"", "\"delta\"");
        assert_corrupt(PatchAssetManifest::parse(bad_enc.as_bytes()).unwrap_err());
    }

    #[test]
    fn enforces_64_char_lowercase_sha256_hex() {
        let upper = VALID_HASH.to_ascii_uppercase();
        let long = format!("{VALID_HASH}0");
        let non_hex = format!("g{}", &VALID_HASH[1..]);
        for bad in ["", &VALID_HASH[..63], &upper, &long, &non_hex] {
            let json = manifest_json_with("assets/logo.png", bad);
            assert_corrupt(PatchAssetManifest::parse(json.as_bytes()).unwrap_err());
        }
    }

    #[test]
    fn enforces_canonical_relative_paths() {
        let root = Path::new("/tmp/patches/1/assets/files");
        assert_eq!(
            asset_path_for_key(root, "packages/foo/fonts/Bar.ttf").unwrap(),
            root.join("packages")
                .join("foo")
                .join("fonts")
                .join("Bar.ttf")
        );
        for bad_key in [
            "",
            ".",
            "..",
            "/etc/passwd",
            "assets/",
            "./assets/logo.png",
            "assets/./logo.png",
            "../escape",
            "assets/../logo.png",
            "assets/../../escape",
            "assets//logo.png",
            r"assets\logo.png",
            r"..\escape",
            "C:/escape",
            "C:escape",
        ] {
            assert_corrupt(asset_path_for_key(root, bad_key).unwrap_err());
            let bytes = serde_json::to_vec(&serde_json::json!({
                "version": SUPPORTED_VERSION,
                "assets": { bad_key: { "hash": VALID_HASH, "size": 1 } }
            }))
            .unwrap();
            assert_corrupt(PatchAssetManifest::parse(&bytes).unwrap_err());
        }
    }

    #[test]
    fn rejects_all_five_poison_keys() {
        assert_eq!(
            POISON_ASSET_KEYS,
            [
                "AssetManifest.json",
                "AssetManifest.bin",
                "AssetManifest.bin.json",
                "FontManifest.json",
                "NOTICES.Z",
            ]
        );
        let root = Path::new("/tmp/patches/1/assets/files");
        for poison in POISON_ASSET_KEYS {
            assert!(is_poison_asset_key(poison));
            let json = manifest_json_with(poison, VALID_HASH);
            assert_corrupt(PatchAssetManifest::parse(json.as_bytes()).unwrap_err());
            assert!(asset_path_for_key(root, poison).is_err());
        }
        let nested = manifest_json_with("assets/docs/NOTICES.Z", VALID_HASH);
        assert!(PatchAssetManifest::parse(nested.as_bytes()).is_ok());
    }

    #[test]
    fn builds_blob_urls_and_asset_paths() {
        let expected = format!("https://cdn.example.com/app/patches/7/blobs/{VALID_HASH}");
        assert_eq!(
            blob_url("https://cdn.example.com/app/patches/7/blobs", VALID_HASH),
            expected
        );
        assert_eq!(
            blob_url("https://cdn.example.com/app/patches/7/blobs/", VALID_HASH),
            expected
        );
        let dir = Path::new("/tmp/patches/7/assets");
        assert_eq!(manifest_path(dir), dir.join("manifest.json"));
        assert_eq!(asset_files_path(dir), dir.join("files"));
    }
}
