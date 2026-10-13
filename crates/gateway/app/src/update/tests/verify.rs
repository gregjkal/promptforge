use crate::update::verify::{RELEASE_PUBLIC_KEY, Verifier};

use super::TestKey;

#[test]
fn the_release_key_is_the_one_workshop_s_updater_trusts() {
    let config: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../workshop/desktop/tauri.conf.json"
    ))
    .expect("tauri.conf.json parses");
    assert_eq!(
        config["plugins"]["updater"]["pubkey"].as_str(),
        Some(RELEASE_PUBLIC_KEY),
        "the gateway's embedded release key must equal plugins.updater.pubkey"
    );
    assert!(
        Verifier::new(
            RELEASE_PUBLIC_KEY,
            &TestKey::generate().sign_version(b"x", "1.0.0"),
            "1.0.0"
        )
        .is_ok(),
        "the release key decodes as a minisign key"
    );
}

fn verify(verifier: &Verifier, data: &[u8]) -> Result<(), String> {
    let mut stream = verifier.stream()?;
    for chunk in data.chunks(3) {
        stream.update(chunk);
    }
    stream.finish()
}

#[test]
fn a_signature_by_the_key_over_the_data_verifies() {
    let key = TestKey::generate();
    let data = b"the gateway archive";
    let verifier =
        Verifier::new(&key.public(), &key.sign_version(data, "0.4.0"), "0.4.0").expect("decode");
    verify(&verifier, data).expect("the signature verifies in chunks");
}

#[test]
fn a_missing_malformed_or_foreign_signature_is_refused() {
    let key = TestKey::generate();
    let data = b"the gateway archive";

    let error = Verifier::new(&key.public(), "", "0.4.0").expect_err("empty");
    assert!(error.contains("signature"), "{error}");
    let error = Verifier::new(&key.public(), "!!", "0.4.0").expect_err("not base64");
    assert!(error.contains("not standard base64"), "{error}");
    let error =
        Verifier::new(&key.public(), "bm90IG1pbmlzaWdu", "0.4.0").expect_err("not minisign");
    assert!(error.contains("not a minisign signature"), "{error}");

    let other = TestKey::generate();
    let verifier =
        Verifier::new(&key.public(), &other.sign_version(data, "0.4.0"), "0.4.0").expect("decodes");
    let error = verify(&verifier, data).expect_err("another key");
    assert!(error.contains("another key"), "{error}");
}

#[test]
fn a_signature_over_other_data_is_refused() {
    let key = TestKey::generate();
    let verifier = Verifier::new(
        &key.public(),
        &key.sign_version(b"original", "0.4.0"),
        "0.4.0",
    )
    .expect("decodes");
    let error = verify(&verifier, b"tampered").expect_err("other data");
    assert!(error.contains("does not match the download"), "{error}");
}

#[test]
fn a_signature_for_another_version_or_none_is_refused() {
    let key = TestKey::generate();
    let data = b"the gateway archive";
    let error = Verifier::new(&key.public(), &key.sign_version(data, "0.3.9"), "0.4.0")
        .expect_err("another version");
    assert!(
        error.contains("version:0.4.0") && error.contains("version:0.3.9"),
        "the refusal names the required and the signed version: {error}"
    );
    let error = Verifier::new(&key.public(), &key.sign(data, "timestamp:1"), "0.4.0")
        .expect_err("no version");
    assert!(error.contains("version:0.4.0"), "{error}");
    let error = Verifier::new(
        &key.public(),
        &key.sign(data, "timestamp:1\tversion:0.4.0-rc1"),
        "0.4.0",
    )
    .expect_err("a version field must match whole");
    assert!(error.contains("version:0.4.0"), "{error}");
}
