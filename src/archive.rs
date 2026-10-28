//! Safe archive creation and format-aware extraction.
//! Writes only to new destinations; archive contents never follow links or escape root.
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use bzip2::read::BzDecoder;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use xz2::read::XzDecoder;

const MAX_EXTRACTED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_FILES: usize = 20_000;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, msg.into())
}
fn unsafe_archive(msg: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, msg.into())
}
fn present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[derive(Debug, Clone)]
pub enum ArchiveJob {
    Pack { sources: Vec<PathBuf>, target: PathBuf },
    Unpack { source: PathBuf, target: PathBuf },
}

impl ArchiveJob {
    pub fn new_pack(mut sources: Vec<PathBuf>, cwd: &Path) -> io::Result<Self> {
        if sources.is_empty() {
            return Err(invalid("Nothing to archive"));
        }
        let cwd = fs::canonicalize(cwd)?;
        sources.sort_by_key(|p| p.components().count());
        let mut roots: Vec<PathBuf> = Vec::new();
        for source in sources {
            let meta = fs::symlink_metadata(&source)?;
            if !(meta.is_dir() || meta.is_file() || meta.file_type().is_symlink()) {
                return Err(invalid(format!("Unsupported item: {}", source.display())));
            }
            let parent = fs::canonicalize(source.parent().ok_or_else(|| invalid("Missing parent"))?)?;
            let name = source.file_name().ok_or_else(|| invalid("Missing filename"))?;
            let absolute = parent.join(name);
            if !absolute.starts_with(&cwd) {
                return Err(invalid("Selected files must be within the current directory"));
            }
            if roots.iter().any(|existing| absolute == *existing || absolute.starts_with(existing)) {
                continue;
            }
            roots.push(absolute);
        }
        let filename = if roots.len() == 1 {
            format!("{}.tar.gz", roots[0].file_name().unwrap().to_string_lossy())
        } else {
            let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            format!("archive-{secs}.tar.gz")
        };
        let target = cwd.join(filename);
        if present(&target)? {
            return Err(io::Error::new(ErrorKind::AlreadyExists, format!("Already exists: {}", target.display())));
        }
        Ok(Self::Pack { sources: roots, target })
    }

    pub fn new_unpack(source: PathBuf, cwd: &Path) -> io::Result<Self> {
        if !fs::symlink_metadata(&source)?.is_file() {
            return Err(invalid("Select a regular archive file to extract"));
        }
        let _ = detect_format(&source)?;
        let stem = archive_stem(&source)?;
        let target = fs::canonicalize(cwd)?.join(stem);
        if present(&target)? {
            return Err(io::Error::new(ErrorKind::AlreadyExists, format!("Already exists: {}", target.display())));
        }
        Ok(Self::Unpack { source, target })
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Pack { sources, .. } => format!("Create .tar.gz from {} item(s)", sources.len()),
            Self::Unpack { source, .. } => format!("Extract {}", source.file_name().unwrap_or(OsStr::new("archive")).to_string_lossy()),
        }
    }

    pub fn target(&self) -> &Path {
        match self {
            Self::Pack { target, .. } | Self::Unpack { target, .. } => target,
        }
    }

    pub fn execute(&self) -> io::Result<String> {
        match self {
            Self::Pack { sources, target } => {
                create_tar_gz(sources, target)?;
                Ok(format!("Created {}", target.file_name().unwrap().to_string_lossy()))
            }
            Self::Unpack { source, target } => {
                extract(source, target)?;
                Ok(format!("Extracted to {}", target.file_name().unwrap().to_string_lossy()))
            }
        }
    }
}

fn archive_stem(path: &Path) -> io::Result<String> {
    let name = path.file_name().ok_or_else(|| invalid("Invalid archive name"))?.to_string_lossy();
    let lower = name.to_ascii_lowercase();
    for suffix in [".tar.gz", ".tar.xz", ".tar.bz2", ".tgz", ".txz", ".tbz2", ".tbz", ".zip", ".tar"] {
        if lower.ends_with(suffix) {
            let stem = &name[..name.len() - suffix.len()];
            if !stem.is_empty() && stem != "." && stem != ".." {
                return Ok(stem.to_string());
            }
        }
    }
    Err(invalid("Unsupported archive format (supported: .tar, .tar.gz, .tar.xz, .tar.bz2, .zip)"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Format { Tar, Gzip, Xz, Bzip2, Zip }

fn detect_format(path: &Path) -> io::Result<Format> {
    let mut file = File::open(path)?;
    let mut header = [0u8; 512];
    let count = file.read(&mut header)?;
    if count >= 4 && header.starts_with(b"PK\x03\x04") {
        return Ok(Format::Zip);
    }
    if count >= 2 && header.starts_with(&[0x1f, 0x8b]) {
        return Ok(Format::Gzip);
    }
    if count >= 6 && header.starts_with(&[0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00]) {
        return Ok(Format::Xz);
    }
    if count >= 3 && header.starts_with(b"BZh") {
        return Ok(Format::Bzip2);
    }
    if count >= 262 && &header[257..262] == b"ustar" {
        return Ok(Format::Tar);
    }
    // Some valid legacy tar archives have no ustar magic.
    if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("tar")) {
        return Ok(Format::Tar);
    }
    Err(invalid("Unsupported archive type (tar, tar.gz, tar.xz, tar.bz2, zip)"))
}

fn create_tar_gz(sources: &[PathBuf], target: &Path) -> io::Result<()> {
    let file = OpenOptions::new().write(true).create_new(true).open(target)?;
    let result = (|| {
        let writer = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(writer);
        builder.follow_symlinks(false); // never archive files outside via symlinks
        for source in sources {
            let name = source.file_name().ok_or_else(|| invalid("Invalid source name"))?;
            if fs::symlink_metadata(source)?.is_dir() {
                builder.append_dir_all(name, source)?;
            } else {
                builder.append_path_with_name(source, name)?;
            }
        }
        let writer = builder.into_inner()?;
        let result_file = writer.finish()?;
        result_file.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(target);
    }
    result
}

/// Resolve an archive member into a path under a freshly created destination.
/// Reject parent traversal, absolute paths, and alternate separators.
fn safe_member(root: &Path, name: &Path) -> io::Result<PathBuf> {
    if name.as_os_str().to_string_lossy().contains('\\') {
        return Err(unsafe_archive("Unsafe archive path contains backslash"));
    }
    let mut result = root.to_path_buf();
    let mut any = false;
    for part in name.components() {
        match part {
            Component::Normal(value) => { result.push(value); any = true; }
            Component::CurDir => {}
            _ => return Err(unsafe_archive(format!("Archive path escapes extraction folder: {}", name.display()))),
        }
    }
    if !any { return Err(unsafe_archive("Invalid empty archive path")); }
    Ok(result)
}

fn extract(source: &Path, target: &Path) -> io::Result<()> {
    let format = detect_format(source)?;
    fs::create_dir(target)?; // exclusive folder: never merge/overwrite
    let result = (|| match format {
        Format::Zip => extract_zip(source, target),
        Format::Tar => extract_tar(File::open(source)?, target),
        Format::Gzip => extract_tar(GzDecoder::new(File::open(source)?), target),
        Format::Xz => extract_tar(XzDecoder::new(File::open(source)?), target),
        Format::Bzip2 => extract_tar(BzDecoder::new(File::open(source)?), target),
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(target); // remove only the folder we created
    }
    result
}

fn copy_bounded(reader: &mut impl Read, output: &mut File, total: &mut u64) -> io::Result<()> {
    let remaining = MAX_EXTRACTED_BYTES.saturating_sub(*total);
    let mut bounded = reader.take(remaining + 1);
    let copied = io::copy(&mut bounded, output)?;
    if copied > remaining {
        return Err(unsafe_archive("Archive exceeds 2 GiB extraction limit"));
    }
    *total += copied;
    Ok(())
}

fn extract_tar<R: Read>(reader: R, root: &Path) -> io::Result<()> {
    let mut archive = tar::Archive::new(reader);
    let mut bytes = 0;
    let mut count = 0usize;
    for item in archive.entries()? {
        count += 1;
        if count > MAX_ARCHIVE_FILES {
            return Err(unsafe_archive("Archive has too many entries"));
        }
        let mut item = item?;
        let name = item.path()?.into_owned();
        let path = safe_member(root, &name)?;
        let kind = item.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&path)?;
        } else if kind.is_file() {
            if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
            let mut output = OpenOptions::new().write(true).create_new(true).open(&path)?;
            copy_bounded(&mut item, &mut output, &mut bytes)?;
        } else {
            // No links, devices, FIFOs, pax surprises, or traversal from untrusted archives.
            return Err(unsafe_archive(format!("Unsupported archive member type: {}", name.display())));
        }
    }
    Ok(())
}

fn extract_zip(source: &Path, root: &Path) -> io::Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(source)?)
        .map_err(|e| unsafe_archive(format!("Invalid ZIP archive: {e}")))?;
    if archive.len() > MAX_ARCHIVE_FILES {
        return Err(unsafe_archive("ZIP archive has too many entries"));
    }
    let mut bytes = 0;
    for index in 0..archive.len() {
        let mut item = archive.by_index(index)
            .map_err(|e| unsafe_archive(format!("Cannot read ZIP entry: {e}")))?;
        let name = item.enclosed_name()
            .ok_or_else(|| unsafe_archive("Unsafe ZIP archive path"))?
            .to_path_buf();
        let path = safe_member(root, &name)?;
        if item.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
            return Err(unsafe_archive("ZIP symlinks are not extracted"));
        }
        if item.is_dir() {
            fs::create_dir_all(path)?;
        } else {
            if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
            let mut output = OpenOptions::new().write(true).create_new(true).open(&path)?;
            copy_bounded(&mut item, &mut output, &mut bytes)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn fixture() -> PathBuf {
        let id = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("rzpk-archive-{}-{id}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        dir
    }
    #[test]
    fn tar_gz_roundtrip_file_and_nested_directory() -> io::Result<()> {
        let root = fixture();
        fs::write(root.join("hello.txt"), "hello")?;
        fs::create_dir(root.join("nested"))?;
        fs::write(root.join("nested/world.rs"), "fn main() {}")?;
        let pack = ArchiveJob::new_pack(vec![root.join("hello.txt"), root.join("nested")], &root)?;
        assert!(pack.target().extension().is_some_and(|ext| ext == "gz"));
        let archive_path = pack.target().to_path_buf();
        pack.execute()?;
        let out = root.join("out");
        fs::create_dir(&out)?;
        let job = ArchiveJob::new_unpack(archive_path, &out)?;
        job.execute()?;
        let target = job.target();
        assert_eq!(fs::read_to_string(target.join("hello.txt"))?, "hello");
        assert_eq!(fs::read_to_string(target.join("nested/world.rs"))?, "fn main() {}");
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn extracts_zip_to_own_folder() -> io::Result<()> {
        let root = fixture();
        let src = root.join("test.zip");
        let file = File::create(&src)?;
        let mut writer = zip::ZipWriter::new(file);
        writer.start_file::<_, ()>("src/code.rs", zip::write::FileOptions::default())
            .map_err(io::Error::other)?;
        writer.write_all(b"code")?;
        writer.finish().map_err(io::Error::other)?;
        let job = ArchiveJob::new_unpack(src, &root)?;
        job.execute()?;
        assert_eq!(fs::read_to_string(root.join("test/src/code.rs"))?, "code");
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn denies_path_escape_and_overwrite() -> io::Result<()> {
        let root = fixture();
        assert!(safe_member(&root, Path::new("../escape")).is_err());
        assert!(safe_member(&root, Path::new("/etc/passwd")).is_err());
        assert!(safe_member(&root, Path::new("..\\evil")).is_err());
        fs::write(root.join("test.tar"), b"")?;
        fs::create_dir(root.join("test"))?;
        assert!(ArchiveJob::new_unpack(root.join("test.tar"), &root).is_err());
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn tar_with_symlink_is_rejected_without_escaping() -> io::Result<()> {
        let root = fixture();
        let target = root.join("evil.tar");
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        builder.append_link(&mut header, "link", "../../escape")?;
        let data = builder.into_inner()?;
        fs::write(&target, data)?;
        let job = ArchiveJob::new_unpack(target, &root)?;
        assert!(job.execute().is_err());
        assert!(!job.target().exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn short_magic_detection() -> io::Result<()> {
        let root = fixture();
        let cases: &[(&str, &[u8], Format)] = &[
            ("gz", &[0x1f, 0x8b, 8], Format::Gzip),
            ("xz", &[0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00], Format::Xz),
            ("bz", b"BZh9", Format::Bzip2),
            ("zip", b"PK\x03\x04", Format::Zip),
        ];
        for (suffix, data, format) in cases {
            let path = root.join(suffix);
            fs::write(&path, data)?;
            assert_eq!(detect_format(&path)?, *format);
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
