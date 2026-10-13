//! Self-update end to end: a copy of the real binary, installed in a
//! temporary directory and pretending through `PROMPTFORGE_GATEWAY_TEST_VERSION`
//! to be older than this build, updates through `POST /update` from a local
//! release server signed with a key generated per run, and hands its lease
//! to the swapped-in copy.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gateway_api_discovery::{GatewayDiscoveryFile, GatewayInstanceLease, ValidatedConnection};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const OLD_VERSION: &str = "0.0.1";
const WAIT: Duration = Duration::from_secs(60);

/// The manifest key of this machine's gateway archive.
fn platform_key() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-aarch64-gateway",
        ("macos", _) => "darwin-x86_64-gateway",
        ("linux", "aarch64") => "linux-aarch64-gateway",
        _ => "linux-x86_64-gateway",
    }
}

/// An installed copy of the gateway, its profile, and the release server.
struct Release {
    temp: tempfile::TempDir,
    installed: PathBuf,
    config: PathBuf,
    manifest_url: String,
    public_key: String,
    runtime: tokio::runtime::Runtime,
}

impl Release {
    /// Installs a copy of the binary and serves a release of this build
    /// signed by `signer`; `trusted` is the key the installed copy
    /// verifies against.
    fn new(signer: &minisign::KeyPair, trusted: &minisign::KeyPair) -> Release {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("install");
        std::fs::create_dir(&install).unwrap();
        let installed = install.join("promptforge-gateway");
        std::fs::copy(env!("CARGO_BIN_EXE_promptforge-gateway"), &installed).unwrap();
        let config = temp.path().join("gateway.toml");
        std::fs::write(
            &config,
            "config-version = 0\n\n[server]\nbind = \"127.0.0.1:0\"\napi_key = \"test-token\"\n",
        )
        .unwrap();

        let archive = archive_of(Path::new(env!("CARGO_BIN_EXE_promptforge-gateway")));
        let signature = minisign::sign(
            Some(&signer.pk),
            &signer.sk,
            archive.as_slice(),
            Some(&format!(
                "timestamp:1\tfile:gateway.tar.gz\tversion:{VERSION}"
            )),
            None,
        )
        .unwrap()
        .into_string();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let address = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let manifest = serde_json::to_vec(&serde_json::json!({
                "version": VERSION,
                "platforms": {
                    platform_key(): {
                        "url": format!("http://{address}/gateway.tar.gz"),
                        "signature": STANDARD.encode(signature),
                    },
                },
            }))
            .unwrap();
            let router = axum::Router::new()
                .route(
                    "/latest.json",
                    axum::routing::get(move || {
                        let manifest = manifest.clone();
                        async move { manifest }
                    }),
                )
                .route(
                    "/gateway.tar.gz",
                    axum::routing::get(move || {
                        let archive = archive.clone();
                        async move { archive }
                    }),
                );
            tokio::spawn(async move {
                let _ = axum::serve(listener, router).await;
            });
            address
        });
        Release {
            temp,
            installed,
            config,
            manifest_url: format!("http://{address}/latest.json"),
            public_key: STANDARD.encode(trusted.pk.to_box().unwrap().into_string()),
            runtime,
        }
    }

    fn run_dir(&self) -> PathBuf {
        self.temp.path().join(".promptforge").join("run")
    }

    /// Starts the installed copy headless, reporting the old version.
    fn start(&self) -> std::process::Child {
        let log = std::fs::File::create(self.temp.path().join("stdout.log")).unwrap();
        std::process::Command::new(&self.installed)
            .arg("--config")
            .arg(&self.config)
            .arg("--print-url")
            .env("USERPROFILE", self.temp.path())
            .env("HOME", self.temp.path())
            .env("PROMPTFORGE_UPDATE_URL", &self.manifest_url)
            .env("PROMPTFORGE_UPDATE_TEST_PUBKEY", &self.public_key)
            .env("PROMPTFORGE_GATEWAY_TEST_VERSION", OLD_VERSION)
            .env_remove("PROMPTFORGE_GATEWAY_CONFIG")
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Posts `POST /update` to the gateway `file` names.
    fn update(&self, file: &GatewayDiscoveryFile) -> (u16, serde_json::Value) {
        self.runtime.block_on(async {
            let response = reqwest::Client::new()
                .post(format!("http://127.0.0.1:{}/update", file.port))
                .bearer_auth(&file.api_key)
                .send()
                .await
                .unwrap();
            let status = response.status().as_u16();
            (status, response.json().await.unwrap())
        })
    }

    fn installed_version(&self) -> String {
        let output = std::process::Command::new(&self.installed)
            .arg("--version")
            .env_remove("PROMPTFORGE_GATEWAY_TEST_VERSION")
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
}

/// Stops whatever gateway the run directory still names, so a failed
/// assertion leaves no successor running.
impl Drop for Release {
    fn drop(&mut self) {
        if let Ok(Some(file)) = GatewayDiscoveryFile::read(&self.run_dir())
            && let Ok(connection) = ValidatedConnection::validate(file)
        {
            let _ = gateway_api_discovery::request_shutdown(&connection);
        }
    }
}

/// A `.tar.gz` holding `exe` as one executable named `promptforge-gateway`.
fn archive_of(exe: &Path) -> Vec<u8> {
    let bytes = std::fs::read(exe).unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o755);
    header.set_entry_type(tar::EntryType::Regular);
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    builder
        .append_data(&mut header, "promptforge-gateway", bytes.as_slice())
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "{what} within {WAIT:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn record(run_dir: &Path) -> Option<GatewayDiscoveryFile> {
    GatewayDiscoveryFile::read(run_dir).ok().flatten()
}

#[test]
fn an_update_hands_the_lease_to_the_swapped_in_gateway() {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let release = Release::new(&key, &key);
    let mut old = release.start();
    let run_dir = release.run_dir();
    wait_for("the old gateway publishes", || {
        record(&run_dir).is_some_and(|file| file.pid == old.id())
    });
    let file = record(&run_dir).unwrap();
    assert_eq!(file.version, OLD_VERSION);

    let (status, answer) = release.update(&file);
    assert_eq!(status, 202, "{answer}");
    assert_eq!(answer["outcome"], "updating");
    assert_eq!(answer["version"], VERSION);

    let mut exited = None;
    wait_for("the old gateway exits after its succession", || {
        exited = old.try_wait().unwrap();
        exited.is_some()
    });
    assert!(exited.unwrap().success(), "the succession succeeded");
    let successor = record(&run_dir).expect("the successor's record");
    assert_ne!(successor.pid, old.id());
    assert_eq!(successor.version, VERSION);
    assert!(!successor.restarting);
    assert!(
        ValidatedConnection::validate(successor).is_ok(),
        "the successor serves its record"
    );
    assert!(
        GatewayInstanceLease::try_acquire(&run_dir)
            .unwrap()
            .is_none(),
        "the successor holds the instance lease"
    );
    assert_eq!(
        release.installed_version(),
        format!("promptforge-gateway {VERSION}")
    );
    let install = release.installed.parent().unwrap();
    assert!(!install.join("promptforge-gateway.previous").exists());
    assert!(!install.join(".promptforge-gateway-update").exists());
}

#[test]
fn a_release_signed_by_another_key_leaves_the_gateway_serving_and_untouched() {
    let signer = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let trusted = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let release = Release::new(&signer, &trusted);
    let before = std::fs::read(&release.installed).unwrap();
    let mut old = release.start();
    let run_dir = release.run_dir();
    wait_for("the gateway publishes", || {
        record(&run_dir).is_some_and(|file| file.pid == old.id())
    });
    let file = record(&run_dir).unwrap();

    let (status, answer) = release.update(&file);
    assert_eq!(status, 502, "{answer}");
    assert_eq!(answer["outcome"], "failed");
    assert!(
        answer["message"]
            .as_str()
            .is_some_and(|message| message.contains(platform_key())),
        "the refusal names the platform key: {answer}"
    );

    assert!(
        old.try_wait().unwrap().is_none(),
        "the gateway keeps serving"
    );
    let still = record(&run_dir).unwrap();
    assert_eq!(still.pid, old.id());
    assert!(!still.restarting);
    assert_eq!(std::fs::read(&release.installed).unwrap(), before);
    let install = release.installed.parent().unwrap();
    assert!(!install.join("promptforge-gateway.previous").exists());
    assert!(!install.join(".promptforge-gateway-update").exists());
    let _ = old.kill();
    let _ = old.wait();
}
