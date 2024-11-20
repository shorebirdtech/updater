// cspell:ignore pubin PKCS outform
use anyhow::{bail, Context, Result};
use base64::Engine;
use std::path::Path;

/// Reads the file at `path` and returns the SHA-256 hash of its contents as a String.
pub fn hash_file<P: AsRef<Path>>(path: P) -> Result<String> {
    use sha2::{Digest, Sha256}; // `Digest` is needed for `Sha256::new()`;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    let hash = hasher.finalize();
    Ok(hex::encode(hash))
}

/// `public_key` is a DER base64-encoded RSA public key.
///
/// Given a public_key.pem file, this can be generated with the following command:
///   openssl rsa -pubin \
///     -in public_key.pem \
///     -inform PEM \
///     -RSAPublicKey_out \
///     -outform DER \
///     -out public_key.der
///
/// See https://docs.rs/ring/latest/ring/signature/index.html#signing-and-verifying-with-rsa-pkcs1-15-padding
/// for more information.
pub fn check_signature(message: &str, signature: &str, public_key: &str) -> Result<()> {
    shorebird_debug!("Message is {}", message);
    shorebird_debug!("Public key is {:?}", public_key);
    shorebird_debug!("Signature is {}", signature);

    let public_key_bytes = base64::prelude::BASE64_STANDARD
        .decode(public_key)
        .with_context(|| format!("Failed to decode public_key: {}", public_key))?;
    let public_key = ring::signature::UnparsedPublicKey::new(
        &ring::signature::RSA_PKCS1_2048_8192_SHA256,
        public_key_bytes,
    );
    let decoded_sig = base64::prelude::BASE64_STANDARD
        .decode(signature)
        .map_err(|e| anyhow::Error::msg(format!("Failed to decode signature: {:?}", e)))?;

    shorebird_info!("Verifying patch signature...");
    match public_key.verify(message.as_bytes(), &decoded_sig) {
        Ok(_) => {
            shorebird_info!("Patch signature is valid");
            Ok(())
        }
        Err(_) => {
            // The error provided by `verify` is (by design) not helpful, so we ignore it.
            // See https://docs.rs/ring/latest/ring/error/struct.Unspecified.html
            shorebird_error!("Patch signature is invalid");
            bail!("Patch signature is invalid")
        }
    }
}

#[cfg(test)]
mod tests {
    // The constant values below were generated by taking an arbitrary hash (`MESSAGE`) and
    // using openssl to sign it with a private key.

    // The base64-encoded public half of the key pair used to sign `MESSAGE`.
    const PUBLIC_KEY: &str = "MIIBCgKCAQEA2wdpEGbuvlPsb9i0qYrfMefJnEw1BHTi8SYZTKrXOvJWmEpPE1hWfbkvYzXu5a96gV1yocF3DMwn04VmRlKhC4AhsD0NL0UNhYhotbKG91Kwi1vAXpHhCdz5gQEBw0K1uB4Jz+zK6WK+31PryYpwLwbyXNqXoY8IAAUQ4STsHYV5w+BMSi8pepWMRd7DR9RHcbNOZlJvdBQ5NxvB4JN4dRMq8cC73ez1P9d7Dfwv3TWY+he9EmuXLT2UivZSlHIrGBa7MFfqyUe2ro0F7Te/B0si12itBbWIqycvqcXjeOPNn6WEpqN7IWjb9LUh162JyYaz5Lb/VeeJX8LKtElccwIDAQAB";

    // The message that was signed.
    const MESSAGE: &str = "404e5caa5b906f6d03c97657e8c4d604d759f9cfba1a8bba9d5b49a5ebc174f9";

    // The base64-encoded signature of `MESSAGE` using the private key corresponding to `PUBLIC_KEY`.
    const SIGNATURE: &str = "2ixSo5LpaWUSLg2GJEV+D+uyLeLjp0c3vNXnl0yb1iJjAdpn10BFlbcwCcjaJW9PNky2HU2hKOBe62PkFHOU8DDYOfxf2LGg/ToLGPHin85WrwFAceAUYDs7JpQr43dRTbrXcT8k5tuCQOTwXecGwuWcOFFvh0GbXFnyAmi7fLfN9CtTsG2GIOle/LyYLwoviTrXn/fZTZEYrqxD/wZ4QzoWOWLWNvrPbILhqWELkBLhdZeK0+nC2CIxFRYd3bUeOi1AGtPyHKBfdwuf4VO3+HbwJVaAEiD7HU2Bj+Zp1xeSdbznmYgBV86oizrLFd23D+lBfTlmDGgdfNE9J4Z2/g==";

    use std::io::Write;

    use anyhow::Result;
    use tempdir::TempDir;

    #[test]
    fn errs_if_file_does_not_exist() {
        let path = "/tmp/does_not_exist";
        let result = super::hash_file(path);
        assert!(result.is_err());
    }

    #[test]
    fn hashes_file_contents() -> Result<()> {
        // Write "hello, world!" to a file.
        let temp_dir = TempDir::new("signing")?;
        let file_path = temp_dir.path().join("test.txt");
        let mut file = std::fs::File::create(&file_path)?;
        file.write_all("hello, world!".as_bytes())?;

        // Verify that the hash is correct.
        let hashed = super::hash_file(file_path)?;
        assert_eq!(
            &hashed,
            "68e656b251e67e8358bef8483ab0d51c6619f3e7a1a9f0e75838d41ff368f728"
        );
        Ok(())
    }

    #[test]
    fn errs_if_public_key_cannot_be_decoded() {
        let result = super::check_signature(MESSAGE, SIGNATURE, "bad_public_key");
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert_eq!(error, "Failed to decode public_key: bad_public_key");
    }

    #[test]
    fn errs_if_signature_cannot_be_decoded() {
        let result = super::check_signature(MESSAGE, "signature", PUBLIC_KEY);
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.starts_with("Failed to decode signature"));
    }

    #[test]
    fn errs_if_signature_is_not_valid() {
        // Pass PUBLIC_KEY as the signature to ensure that the signature is invalid.
        let result = super::check_signature(MESSAGE, PUBLIC_KEY, PUBLIC_KEY);
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.starts_with("Patch signature is invalid"));
    }

    #[test]
    fn is_ok_if_signature_is_valid() {
        let result = super::check_signature(MESSAGE, SIGNATURE, PUBLIC_KEY);
        assert!(result.is_ok());
    }
}
