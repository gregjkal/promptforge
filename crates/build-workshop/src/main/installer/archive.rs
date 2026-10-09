//! The Gateway updater archive: a gzipped tar holding one payload item at
//! its root, the bare Gateway binary on Linux or `PromptForge Gateway.app`
//! on macOS.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write as _};
use std::path::Path;

use flate2::Compression;
use flate2::write::GzEncoder;

/// Writes `item`, a file or a directory tree, at the root of `archive`.
/// Entries come in sorted path order with a zero mtime and owner, so the
/// same payload gives the same archive. Directories and executable files
/// get mode `0755`, other files `0644`; anything else, such as a symlink,
/// is refused naming its path.
pub(crate) fn write_archive(item: &Path, archive: &Path) -> io::Result<()> {
    let name = item.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no file name to archive under", item.display()),
        )
    })?;
    let encoder = GzEncoder::new(
        BufWriter::new(File::create(archive)?),
        Compression::default(),
    );
    let mut builder = tar::Builder::new(encoder);
    append(&mut builder, item, Path::new(name))?;
    let mut writer = builder.into_inner()?.finish()?;
    writer.flush()
}

fn append(builder: &mut tar::Builder<impl io::Write>, path: &Path, name: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let mut header = tar::Header::new_gnu();
    header.set_mtime(0);
    if metadata.is_dir() {
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        builder.append_data(&mut header, name, io::empty())?;
        let mut children = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        children.sort();
        for child in children {
            append(builder, &path.join(&child), &name.join(&child))?;
        }
        Ok(())
    } else if metadata.is_file() {
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(metadata.len());
        header.set_mode(if is_executable(&metadata) {
            0o755
        } else {
            0o644
        });
        builder.append_data(&mut header, name, File::open(path)?)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is neither a file nor a directory and cannot be archived",
                path.display()
            ),
        ))
    }
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

/// Archives are written only for macOS and Linux targets, which build on
/// those systems.
#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}
