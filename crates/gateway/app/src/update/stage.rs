//! Staging an update beside the install: the installed item, the staging
//! directory, the verified download, the extraction, and the staged
//! executable's version check.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use gateway_progress::Activity;

use super::UpdateError;
use super::verify::Verifier;

/// The staging directory's name, beside the installed item and so on its
/// filesystem, which a rename needs.
pub(crate) const STAGING_DIR_NAME: &str = ".promptforge-gateway-update";

/// The largest download accepted. The macOS bundle and the Linux binary are
/// tens of megabytes; the Windows setup carries Workshop too.
pub(crate) const DOWNLOAD_CAP: u64 = 512 * 1024 * 1024;

/// How long a download may go without a byte before it fails.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the staged executable may take to print its version.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// What the update replaces: the `.app` bundle holding the executable on
/// macOS, the executable itself elsewhere. Read from the executable's path
/// before the swap: on Linux the running path follows the moved-aside file
/// afterward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Install {
    item: PathBuf,
    within: PathBuf,
}

impl Install {
    /// The install the executable at `exe` belongs to.
    pub(crate) fn locate(exe: &Path) -> Install {
        match gateway_api_discovery::app_bundle(exe) {
            Some(bundle) => Install {
                item: bundle.to_path_buf(),
                within: exe
                    .strip_prefix(bundle)
                    .map(Path::to_path_buf)
                    .unwrap_or_default(),
            },
            None => Install {
                item: exe.to_path_buf(),
                within: PathBuf::new(),
            },
        }
    }

    /// The installed item: the bundle or the executable.
    pub(crate) fn item(&self) -> &Path {
        &self.item
    }

    /// The executable inside `item`, an item shaped like the installed one.
    pub(crate) fn executable_in(&self, item: &Path) -> PathBuf {
        if self.within.as_os_str().is_empty() {
            item.to_path_buf()
        } else {
            item.join(&self.within)
        }
    }

    /// The installed executable.
    pub(crate) fn executable(&self) -> PathBuf {
        self.executable_in(&self.item)
    }

    /// Refuses an executable inside another application's bundle, such as
    /// Workshop's: the swap would replace that application, and the
    /// succession's clean-up would then delete it.
    ///
    /// # Errors
    /// Returns [`UpdateError::NotStandalone`] naming the bundle.
    pub(crate) fn require_standalone(&self) -> Result<(), UpdateError> {
        let bundled = !self.within.as_os_str().is_empty();
        let own_bundle = self.item.file_name()
            == Some(std::ffi::OsStr::new(
                gateway_api_discovery::GATEWAY_BUNDLE_NAME,
            ));
        if bundled && !own_bundle {
            return Err(UpdateError::NotStandalone {
                bundle: self.item.clone(),
            });
        }
        Ok(())
    }
}

/// The staging directory, removed on drop unless the swap took it over.
#[derive(Debug)]
pub(crate) struct Staging {
    dir: Option<PathBuf>,
}

impl Staging {
    /// Creates the staging directory fresh beside `install`'s item,
    /// removing what an earlier failed update left there.
    ///
    /// # Errors
    /// Returns [`UpdateError::NotWritable`] naming the directory when it
    /// cannot be cleared or created.
    pub(crate) fn create(install: &Install) -> Result<Staging, UpdateError> {
        let parent = install.item.parent().unwrap_or_else(|| Path::new("."));
        let dir = parent.join(STAGING_DIR_NAME);
        let not_writable = |source| UpdateError::NotWritable {
            directory: dir.clone(),
            source,
        };
        match remove_item(&dir) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                return Err(not_writable(error));
            }
            _ => {}
        }
        fs::create_dir(&dir).map_err(not_writable)?;
        Ok(Staging { dir: Some(dir) })
    }

    fn dir(&self) -> &Path {
        self.dir.as_deref().unwrap_or_else(|| Path::new(""))
    }

    /// Where the download lands.
    pub(crate) fn archive(&self) -> PathBuf {
        self.dir().join("download.tar.gz")
    }

    /// Where the archive extracts.
    pub(crate) fn extracted(&self) -> PathBuf {
        self.dir().join("extracted")
    }

    /// Hands the directory to the succession, which removes it once the
    /// outcome is known.
    pub(crate) fn into_path(mut self) -> PathBuf {
        self.dir.take().unwrap_or_default()
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take()
            && let Err(error) = fs::remove_dir_all(&dir)
        {
            tracing::warn!(
                "could not remove the update staging {}: {error}",
                dir.display()
            );
        }
    }
}

/// Removes a file or a directory tree.
pub(crate) fn remove_item(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// One download to verify while it streams.
#[derive(Debug)]
pub(crate) struct Download<'a> {
    /// What to fetch.
    pub(crate) url: &'a str,
    /// The manifest key it came from, for error text.
    pub(crate) key: &'static str,
    /// The signature check it must pass.
    pub(crate) verifier: &'a Verifier,
    /// The progress activity whose text shows the percentage.
    pub(crate) activity: &'a Activity,
    /// The activity text before the percentage.
    pub(crate) label: String,
}

/// Downloads `download` into `to`, at most [`DOWNLOAD_CAP`] bytes, hashing
/// every chunk for the signature check and failing unless it verifies.
///
/// # Errors
/// Returns [`UpdateError::Download`] for a transport, status, size, or
/// write failure, and [`UpdateError::Signature`] when the signature does
/// not verify. Either way nothing stays at `to`.
pub(crate) async fn download(download: Download<'_>, to: &Path) -> Result<(), UpdateError> {
    download_within(download, to, STALL_TIMEOUT).await
}

/// [`download`], failing once no byte arrives for `stall`.
pub(crate) async fn download_within(
    download: Download<'_>,
    to: &Path,
    stall: Duration,
) -> Result<(), UpdateError> {
    let result = fetch(download, to, stall).await;
    if result.is_err() {
        // The Windows setup lands in the temporary directory, which no
        // staging guard removes, so unverified bytes go here.
        let _ = fs::remove_file(to);
    }
    result
}

async fn fetch(download: Download<'_>, to: &Path, stall: Duration) -> Result<(), UpdateError> {
    let failed = |reason: String| UpdateError::Download {
        url: download.url.to_owned(),
        reason,
    };
    let signature = |reason| UpdateError::Signature {
        key: download.key,
        reason,
    };
    let mut stream = download.verifier.stream().map_err(signature)?;
    let mut response = gateway_protocol::http_util::streaming_client()
        .get(download.url)
        .send()
        .await
        .map_err(|error| failed(transport(error)))?;
    let status = response.status();
    if !status.is_success() {
        return Err(failed(format!(
            "the server answered {status}, required a 2xx success"
        )));
    }
    let total = response.content_length();
    if total.is_some_and(|total| total > DOWNLOAD_CAP) {
        return Err(failed(format!(
            "it is {} bytes; at most {DOWNLOAD_CAP} are accepted",
            total.unwrap_or_default()
        )));
    }
    let file = fs::File::create(to)
        .map_err(|error| failed(format!("cannot create {}: {error}", to.display())))?;
    let mut writer = io::BufWriter::new(file);
    let mut received: u64 = 0;
    let mut shown = None;
    // The streaming client bounds only the connect, so a peer that holds
    // the connection open and stops sending would hold the update forever.
    while let Some(chunk) = tokio::time::timeout(stall, response.chunk())
        .await
        .map_err(|_| {
            failed(format!(
                "nothing arrived for {stall:?} after {received} bytes"
            ))
        })?
        .map_err(|error| failed(transport(error)))?
    {
        received += chunk.len() as u64;
        if received > DOWNLOAD_CAP {
            return Err(failed(format!("it exceeds the {DOWNLOAD_CAP}-byte limit")));
        }
        stream.update(&chunk);
        writer
            .write_all(&chunk)
            .map_err(|error| failed(format!("cannot write {}: {error}", to.display())))?;
        if let Some(percent) = percent(received, total)
            && shown != Some(percent)
        {
            shown = Some(percent);
            download
                .activity
                .set_text(format!("{} {percent}%", download.label));
        }
    }
    writer
        .flush()
        .map_err(|error| failed(format!("cannot write {}: {error}", to.display())))?;
    stream.finish().map_err(signature)
}

/// A transport error with its causes, without the URL the message
/// already names.
fn transport(error: reqwest::Error) -> String {
    crate::error::error_chain(&error.without_url())
}

fn percent(received: u64, total: Option<u64>) -> Option<u64> {
    total
        .filter(|total| *total > 0)
        .map(|total| received.saturating_mul(100) / total)
}

/// Extracts the `.tar.gz` at `archive` into `into` and returns its one
/// top-level item. Only regular files and directories are accepted, and
/// every entry must stay inside `into`.
///
/// # Errors
/// Returns [`UpdateError::Archive`] naming the refused entry or the
/// failure.
pub(crate) fn extract(
    archive: &Path,
    into: &Path,
    key: &'static str,
) -> Result<PathBuf, UpdateError> {
    let refused = |reason: String| UpdateError::Archive { key, reason };
    let file = fs::File::open(archive)
        .map_err(|error| refused(format!("cannot open {}: {error}", archive.display())))?;
    fs::create_dir_all(into)
        .map_err(|error| refused(format!("cannot create {}: {error}", into.display())))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut top = None;
    for entry in tar
        .entries()
        .map_err(|error| refused(format!("it is not a .tar.gz: {error}")))?
    {
        let mut entry = entry.map_err(|error| refused(format!("it is not a .tar.gz: {error}")))?;
        let path = entry
            .path()
            .map_err(|error| refused(format!("an entry has an unreadable path: {error}")))?
            .into_owned();
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(refused(format!(
                "{} is a {kind:?} entry; only files and directories are accepted",
                path.display()
            )));
        }
        let mut components = path.components().filter(|part| *part != Component::CurDir);
        let Some(Component::Normal(first)) = components.next() else {
            return Err(refused(format!("{} leaves the archive", path.display())));
        };
        if components.any(|part| !matches!(part, Component::Normal(_))) {
            return Err(refused(format!("{} leaves the archive", path.display())));
        }
        match &top {
            None => top = Some(first.to_owned()),
            Some(name) if name == first => {}
            Some(name) => {
                return Err(refused(format!(
                    "it holds {} and {}; one top-level item is required",
                    Path::new(name).display(),
                    Path::new(first).display()
                )));
            }
        }
        let destination = into.join(&path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| refused(format!("cannot create {}: {error}", parent.display())))?;
        }
        entry
            .unpack(&destination)
            .map_err(|error| refused(format!("cannot extract {}: {error}", path.display())))?;
    }
    top.map(|name| into.join(name))
        .ok_or_else(|| refused("it is empty".to_owned()))
}

/// Requires the staged executable to print `promptforge-gateway <version>`
/// for `--version`.
///
/// # Errors
/// Returns [`UpdateError::StagedVersion`] with what it printed, or why it
/// could not run.
pub(crate) fn require_version(exe: &Path, version: &str) -> Result<(), UpdateError> {
    let required = format!("promptforge-gateway {version}");
    let actual = match printed_version(exe) {
        Ok(printed) => printed,
        Err(error) => format!("no version: {} did not run: {error}", exe.display()),
    };
    if actual == required {
        Ok(())
    } else {
        Err(UpdateError::StagedVersion { required, actual })
    }
}

fn printed_version(exe: &Path) -> io::Result<String> {
    let mut child = super::child_command(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + VERSION_TIMEOUT;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("it printed nothing within {VERSION_TIMEOUT:?}"),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
