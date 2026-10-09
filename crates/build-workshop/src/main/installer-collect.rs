//! Moves Tauri's bundle output and copies the Gateway binary into the
//! installer output directory under stable names, and writes the Gateway
//! updater archive.
//!
//! `publish/` holds what a release uploads; `payload/` (macOS and Linux)
//! holds what the installer packages install, under their installed names.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;

use super::{Arch, Platform, System, VERSION};

const PRODUCT: &str = "PromptForge";
const GATEWAY: &str = "promptforge-gateway";

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Collected {
    /// The unsigned Gateway updater archive, written only under `--sign`.
    pub(crate) gateway_archive: Option<PathBuf>,
}

pub(crate) fn collect(
    release: &Path,
    platform: Platform,
    sign: bool,
    output: &Path,
) -> Result<Collected, String> {
    match fs::remove_dir_all(output) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot clear {}: {error}", output.display())),
    }
    let publish = output.join("publish");
    create_directory(&publish)?;
    let bundle = release.join("bundle").join(platform.bundle_directory());
    match platform.system {
        System::Windows => {
            let setup = single_file(&bundle, "-setup.exe")?;
            let published = publish.join(format!(
                "{PRODUCT}_{VERSION}_{}-setup.exe",
                windows_arch(platform.arch)
            ));
            if sign {
                move_path(&signature_path(&setup), &signature_path(&published))?;
            }
            move_path(&setup, &published)?;
            Ok(Collected {
                gateway_archive: None,
            })
        }
        System::MacOs => {
            let payload = output.join("payload");
            create_directory(&payload)?;
            if sign {
                let archive = bundle.join(format!("{PRODUCT}.app.tar.gz"));
                let published = publish.join(format!(
                    "{PRODUCT}_{VERSION}_{}.app.tar.gz",
                    arch_name(platform.arch)
                ));
                move_path(&signature_path(&archive), &signature_path(&published))?;
                move_path(&archive, &published)?;
            }
            let app = format!("{PRODUCT}.app");
            move_path(&bundle.join(&app), &payload.join(&app))?;
            collect_gateway(release, platform, sign, &payload, &publish)
        }
        System::Linux => {
            let payload = output.join("payload");
            create_directory(&payload)?;
            let appimage = single_file(&bundle, ".AppImage")?;
            if sign {
                let published = publish.join(format!(
                    "{PRODUCT}_{VERSION}_{}.AppImage",
                    linux_arch(platform.arch)
                ));
                move_path(&signature_path(&appimage), &signature_path(&published))?;
                copy_file(&appimage, &published)?;
            }
            move_path(&appimage, &payload.join(format!("{PRODUCT}.AppImage")))?;
            collect_gateway(release, platform, sign, &payload, &publish)
        }
    }
}

pub(crate) fn signature_path(path: &Path) -> PathBuf {
    let mut signature = OsString::from(path.as_os_str());
    signature.push(".sig");
    PathBuf::from(signature)
}

/// One entry, the Gateway binary at the archive root with mode `0755`; a
/// zero mtime and owner keep the archive the same for the same binary.
pub(crate) fn write_gateway_archive(binary: &Path, archive: &Path) -> io::Result<()> {
    let mut source = File::open(binary)?;
    let length = source.metadata()?.len();
    let encoder = GzEncoder::new(
        BufWriter::new(File::create(archive)?),
        Compression::default(),
    );
    let mut builder = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(length);
    header.set_mode(0o755);
    header.set_mtime(0);
    builder.append_data(&mut header, GATEWAY, &mut source)?;
    let mut writer = builder.into_inner()?.finish()?;
    writer.flush()
}

fn collect_gateway(
    release: &Path,
    platform: Platform,
    sign: bool,
    payload: &Path,
    publish: &Path,
) -> Result<Collected, String> {
    let gateway = payload.join(GATEWAY);
    copy_file(&release.join(GATEWAY), &gateway)?;
    if !sign {
        return Ok(Collected {
            gateway_archive: None,
        });
    }
    let os = match platform.system {
        System::MacOs => "darwin",
        System::Linux => "linux",
        System::Windows => "windows",
    };
    let archive = publish.join(format!(
        "{GATEWAY}_{VERSION}_{os}-{}.tar.gz",
        arch_name(platform.arch)
    ));
    write_gateway_archive(&gateway, &archive)
        .map_err(|error| format!("cannot write {}: {error}", archive.display()))?;
    Ok(Collected {
        gateway_archive: Some(archive),
    })
}

fn single_file(directory: &Path, suffix: &str) -> Result<PathBuf, String> {
    let entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "cannot read the Tauri bundle directory {}: {error}",
            directory.display()
        )
    })?;
    let mut matches = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
            .path();
        let named = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(suffix));
        if named && path.is_file() {
            matches.push(path);
        }
    }
    match matches.as_slice() {
        [path] => Ok(path.clone()),
        _ => Err(format!(
            "expected exactly one `*{suffix}` in {}, found {}",
            directory.display(),
            matches.len()
        )),
    }
}

fn move_path(source: &Path, destination: &Path) -> Result<(), String> {
    if !source.exists() {
        return Err(format!(
            "{} is missing from the Tauri bundle output",
            source.display()
        ));
    }
    fs::rename(source, destination).map_err(|error| {
        format!(
            "cannot move {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    fs::copy(source, destination).map(|_| ()).map_err(|error| {
        format!(
            "cannot copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })
}

fn create_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| format!("cannot create {}: {error}", path.display()))
}

fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
    }
}

/// Tauri's NSIS and AppImage file names use these spellings.
fn windows_arch(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x64",
        Arch::Aarch64 => "arm64",
    }
}

fn linux_arch(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "amd64",
        Arch::Aarch64 => "aarch64",
    }
}
