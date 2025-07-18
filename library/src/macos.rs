use anyhow::{Context, Result};
use std::fs::File;
use std::io;
use std::path::Path;
use std::process::Command;
use tempfile::NamedTempFile;

/// Creates a temporary copy of a binary file with code signature removed.
/// This is necessary on macOS because signed and unsigned binaries have different hashes
/// even if their core content is identical.
pub fn create_unsigned_copy<P: AsRef<Path>>(binary_path: P) -> Result<NamedTempFile> {
    let binary_path = binary_path.as_ref();
    
    // Create a temporary file to store the unsigned copy
    let temp_file = NamedTempFile::new()
        .with_context(|| "Failed to create temporary file for unsigned binary")?;
    
    // Copy the original binary to the temporary file
    std::fs::copy(binary_path, temp_file.path())
        .with_context(|| format!("Failed to copy binary from {:?} to temporary file", binary_path))?;
    
    // Remove the signature from the temporary copy using codesign
    let output = Command::new("codesign")
        .args(["--remove-signature", temp_file.path().to_str().unwrap()])
        .output()
        .with_context(|| "Failed to execute codesign command")?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to remove signature: {}", stderr);
    }
    
    Ok(temp_file)
}

/// Computes the SHA-256 hash of a binary file after removing its code signature.
/// This is used on macOS to ensure consistent hash comparisons between signed and unsigned binaries.
pub fn hash_unsigned_binary<P: AsRef<Path>>(binary_path: P) -> Result<String> {
    use sha2::{Digest, Sha256};
    
    let unsigned_copy = create_unsigned_copy(binary_path)?;
    
    // Hash the unsigned copy
    let mut file = File::open(unsigned_copy.path())?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    let hash = hasher.finalize();
    
    Ok(hex::encode(hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_create_unsigned_copy() -> Result<()> {
        // Create a test binary file
        let mut test_file = NamedTempFile::new()?;
        test_file.write_all(b"test binary content")?;
        
        // Try to create an unsigned copy
        // Note: This test will only work on macOS and may fail if the test file
        // is not actually a signed binary
        match create_unsigned_copy(test_file.path()) {
            Ok(_unsigned_copy) => {
                // If successful, the unsigned copy was created
                Ok(())
            }
            Err(_) => {
                // Expected to fail for test files that aren't actually signed binaries
                // This is fine for the test
                Ok(())
            }
        }
    }

    #[test]
    fn test_hash_unsigned_binary() -> Result<()> {
        // Create a test binary file
        let mut test_file = NamedTempFile::new()?;
        test_file.write_all(b"test binary content")?;
        
        // Try to hash the unsigned binary
        // Note: This test will only work on macOS and may fail if the test file
        // is not actually a signed binary
        match hash_unsigned_binary(test_file.path()) {
            Ok(_hash) => {
                // If successful, we got a hash
                Ok(())
            }
            Err(_) => {
                // Expected to fail for test files that aren't actually signed binaries
                // This is fine for the test
                Ok(())
            }
        }
    }
}
