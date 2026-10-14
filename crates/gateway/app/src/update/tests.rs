//! Unit tests for the self-update, with fixtures shared by its parts: a
//! signing key generated per run and a local release server.

use std::net::SocketAddr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

mod apply;
mod manifest;
mod stage;
mod succession;
mod swap;
mod verify;
mod windows;

/// A minisign key pair generated for one test, never the release key.
pub(super) struct TestKey {
    pair: minisign::KeyPair,
}

impl TestKey {
    pub(super) fn generate() -> TestKey {
        TestKey {
            pair: minisign::KeyPair::generate_unencrypted_keypair().expect("generate a key pair"),
        }
    }

    /// The public key as `tauri.conf.json` writes one.
    pub(super) fn public(&self) -> String {
        STANDARD.encode(self.pair.pk.to_box().expect("box the key").into_string())
    }

    /// A signature over `data` as the release signer writes one, with
    /// `trusted` as its trusted comment, encoded as the manifest carries it.
    pub(super) fn sign(&self, data: &[u8], trusted: &str) -> String {
        let signature = minisign::sign(
            Some(&self.pair.pk),
            &self.pair.sk,
            data,
            Some(trusted),
            None,
        )
        .expect("sign");
        STANDARD.encode(signature.into_string())
    }

    /// A signature bound to `version`, as `tauri signer sign --app-version`
    /// writes it.
    pub(super) fn sign_version(&self, data: &[u8], version: &str) -> String {
        self.sign(
            data,
            &format!("timestamp:1\tfile:gateway.tar.gz\tversion:{version}"),
        )
    }
}

/// Serves `files` (path, status, body) on a loopback port for the rest of
/// the test's runtime.
pub(super) async fn serve(files: Vec<(&'static str, u16, Vec<u8>)>) -> SocketAddr {
    let mut router = axum::Router::new();
    for (path, status, body) in files {
        let status = axum::http::StatusCode::from_u16(status).expect("a status");
        router = router.route(
            path,
            axum::routing::get(move || {
                let body = body.clone();
                async move { (status, body) }
            }),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the release server");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    address
}

/// A `latest.json` advertising `version` with one `key` entry.
pub(super) fn manifest_json(version: &str, key: &str, url: &str, signature: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "version": version,
        "notes": "",
        "pub_date": "2026-10-13T00:00:00Z",
        "platforms": { key: { "url": url, "signature": signature } },
    }))
    .expect("serialize the manifest")
}

/// A `.tar.gz` of `entries` (path, contents, mode), written with the
/// gateway's own `tar` and `flate2`.
pub(super) fn tar_gz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (path, contents, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(*mode);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, path, *contents)
            .expect("append an entry");
    }
    builder
        .into_inner()
        .expect("finish the tar")
        .finish()
        .expect("finish the gzip")
}
