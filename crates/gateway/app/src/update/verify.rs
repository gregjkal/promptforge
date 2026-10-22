//! The release signature check: the minisign key the release is signed
//! with, and a verifier that hashes a download as it streams in.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// The release public key, as `plugins.updater.pubkey` in Workshop's
/// `tauri.conf.json` writes it: standard base64 of the minisign public key
/// text. A unit test keeps the two equal.
pub(crate) const RELEASE_PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEU3QUNCQ0M4Mzk4M0YwMEEKUldRSzhJTTV5THlzNXpnRXFQellRelNSNm1rTWJiNkNLL0FoL3Y4UFdLeGxRajJJTG96RGFNNEwK";

/// Replaces [`RELEASE_PUBLIC_KEY`] in `test-fixtures` builds, so a test can
/// sign its fixtures with a key generated per run.
#[cfg(feature = "test-fixtures")]
const TEST_PUBLIC_KEY_ENV: &str = "RUNVIL_UPDATE_TEST_PUBKEY";

/// The key updates must verify against.
pub(crate) fn release_key() -> String {
    #[cfg(feature = "test-fixtures")]
    if let Some(key) = std::env::var(TEST_PUBLIC_KEY_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    {
        return key;
    }
    RELEASE_PUBLIC_KEY.to_owned()
}

/// A decoded key and signature, checked to name the advertised version.
pub(crate) struct Verifier {
    key: minisign_verify::PublicKey,
    signature: minisign_verify::Signature,
}

impl std::fmt::Debug for Verifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Verifier")
            .field("trusted_comment", &self.signature.trusted_comment())
            .finish_non_exhaustive()
    }
}

impl Verifier {
    /// Decodes `public_key` and `signature`, each standard base64 of the
    /// minisign text as the Tauri updater reads them, and requires the
    /// signature's trusted comment to carry `version:<version>`, which the
    /// release signer writes. The comment is authenticated by
    /// [`Stream::finish`].
    ///
    /// # Errors
    /// Returns the reason either cannot be used.
    pub(crate) fn new(
        public_key: &str,
        signature: &str,
        version: &str,
    ) -> Result<Verifier, String> {
        let key = minisign_verify::PublicKey::decode(&base64_text(public_key, "public key")?)
            .map_err(|error| format!("the public key is not a minisign key: {error}"))?;
        let signature =
            minisign_verify::Signature::decode(&base64_text(signature, "signature")?)
                .map_err(|error| format!("the signature is not a minisign signature: {error}"))?;
        let required = format!("version:{version}");
        if !signature
            .trusted_comment()
            .split('\t')
            .any(|field| field == required)
        {
            return Err(format!(
                "the signature's trusted comment must carry \"{required}\"; it reads \"{}\"",
                signature.trusted_comment()
            ));
        }
        Ok(Verifier { key, signature })
    }

    /// Starts hashing a download.
    ///
    /// # Errors
    /// Returns the reason when the signature names another key or is not
    /// the prehashed form a streamed check needs.
    pub(crate) fn stream(&self) -> Result<Stream<'_>, String> {
        self.key
            .verify_stream(&self.signature)
            .map(Stream)
            .map_err(|error| reason(&error))
    }
}

/// A download being hashed for its signature check.
pub(crate) struct Stream<'a>(minisign_verify::StreamVerifier<'a>);

impl Stream<'_> {
    /// Hashes the next chunk.
    pub(crate) fn update(&mut self, chunk: &[u8]) {
        self.0.update(chunk);
    }

    /// Checks the signature over everything hashed.
    ///
    /// # Errors
    /// Returns the reason when the signature does not match.
    pub(crate) fn finish(mut self) -> Result<(), String> {
        self.0.finalize().map_err(|error| reason(&error))
    }
}

fn reason(error: &minisign_verify::Error) -> String {
    match error {
        minisign_verify::Error::UnexpectedKeyId => {
            "the signature was made with another key".to_owned()
        }
        minisign_verify::Error::InvalidSignature => {
            "the signature does not match the download".to_owned()
        }
        other => format!("the signature cannot be checked: {other}"),
    }
}

fn base64_text(encoded: &str, what: &str) -> Result<String, String> {
    let bytes = STANDARD
        .decode(encoded.trim())
        .map_err(|error| format!("the {what} is not standard base64: {error}"))?;
    String::from_utf8(bytes).map_err(|_| format!("the {what} does not decode to text"))
}
