//! Safe output files.
//!
//! Every command that writes a path checks it with [`check_output`] before
//! anything is read or created, and writes through [`Staged`], so an existing
//! output is replaced only after the new one is complete.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use opensqlany::SA_MAGIC;

/// Offset of the SQL Anywhere superblock magic (`u32`, little endian).
const MAGIC_OFFSET: usize = 0x14;

/// `true` when `path` is a file that starts with a SQL Anywhere superblock,
/// that is, a QuickBooks company file or another SQL Anywhere database.
fn has_superblock_magic(path: &Path) -> Result<bool> {
    let mut prefix = Vec::with_capacity(MAGIC_OFFSET + 4);
    File::open(path)
        .with_context(|| format!("reading {path:?}"))?
        .take((MAGIC_OFFSET + 4) as u64)
        .read_to_end(&mut prefix)
        .with_context(|| format!("reading {path:?}"))?;
    Ok(prefix
        .get(MAGIC_OFFSET..MAGIC_OFFSET + 4)
        .is_some_and(|m| u32::from_le_bytes([m[0], m[1], m[2], m[3]]) == SA_MAGIC))
}

/// `true` when both paths are the same file: the same path after resolving
/// links, or a hard link to it.
fn same_file(a: &Path, b: &Path) -> bool {
    if let (Ok(x), Ok(y)) = (fs::canonicalize(a), fs::canonicalize(b))
        && x == y
    {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (fs::metadata(a), fs::metadata(b)) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
    }
    false
}

/// Refuse an output `path` that would destroy data: the input itself, or an
/// existing file that starts with the SQL Anywhere superblock magic (a
/// company file named by mistake, for example with the arguments swapped).
/// Touches nothing. A path that does not exist passes.
pub fn check_output(input: &Path, path: &Path) -> Result<()> {
    let Ok(meta) = fs::metadata(path) else {
        return Ok(());
    };
    if same_file(input, path) {
        anyhow::bail!("refusing to write {path:?}: it is the input file. Nothing was changed.");
    }
    if meta.is_file() && has_superblock_magic(path)? {
        anyhow::bail!(
            "refusing to overwrite {path:?}: it is a QuickBooks company file (or another SQL \
             Anywhere database). Check the argument order: the input comes first, the output \
             second. Nothing was changed."
        );
    }
    Ok(())
}

/// A file created next to `dest` and renamed over it by [`commit`](Self::commit).
/// Dropped without a commit, it removes its file (and any SQLite sidecars).
pub struct Staged {
    temp: PathBuf,
    dest: PathBuf,
    done: bool,
}

impl Staged {
    /// A staged file for `dest`, in the same directory so the final rename
    /// stays on one filesystem. Nothing is created yet.
    pub fn new(dest: &Path) -> Self {
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "output".into());
        let temp = dest.with_file_name(format!(".{name}.openqbw-{}.tmp", std::process::id()));
        Self {
            temp,
            dest: dest.to_owned(),
            done: false,
        }
    }

    /// Path to write to.
    pub fn path(&self) -> &Path {
        &self.temp
    }

    /// Write `bytes` to the staged file.
    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        let mut f =
            File::create(&self.temp).with_context(|| format!("creating {:?}", self.temp))?;
        f.write_all(bytes)
            .and_then(|_| f.sync_all())
            .with_context(|| format!("writing {:?}", self.temp))
    }

    /// Replace the destination, a SQLite database, with the staged file. A
    /// `-journal`, `-wal` or `-shm` left next to the destination by an
    /// earlier database is removed first: SQLite would replay it onto the
    /// new file.
    pub fn commit_sqlite(self) -> Result<()> {
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut side = self.dest.clone().into_os_string();
            side.push(suffix);
            let side = PathBuf::from(side);
            match fs::remove_file(&side) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("removing {side:?}")),
            }
        }
        self.commit()
    }

    /// Replace the destination with the staged file.
    pub fn commit(mut self) -> Result<()> {
        fs::rename(&self.temp, &self.dest)
            .with_context(|| format!("replacing {:?} with {:?}", self.dest, self.temp))?;
        self.done = true;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let _ = fs::remove_file(&self.temp);
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut side = self.temp.clone().into_os_string();
            side.push(suffix);
            let _ = fs::remove_file(PathBuf::from(side));
        }
    }
}

/// Several files written together: all staged first, then renamed into
/// place, so a failure while staging leaves every destination as it was.
#[derive(Default)]
pub struct StagedSet(Vec<Staged>);

impl StagedSet {
    /// Stage `bytes` for `dest`.
    pub fn add(&mut self, dest: &Path, bytes: &[u8]) -> Result<()> {
        let staged = Staged::new(dest);
        staged.write(bytes)?;
        self.0.push(staged);
        Ok(())
    }

    /// Rename every staged file over its destination.
    pub fn commit(self) -> Result<()> {
        for s in self.0 {
            s.commit()?;
        }
        Ok(())
    }
}

/// A private temporary directory, removed with its contents when dropped,
/// on success and on every error path.
pub struct TempDir(PathBuf);

impl TempDir {
    /// Create a new directory under the system temp directory. It fails
    /// when the name is taken, so an existing path is never reused.
    pub fn new(prefix: &str) -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        // `mode` below needs `mut` on Unix only.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .with_context(|| format!("creating temporary directory {path:?}"))?;
        Ok(Self(path))
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("openqbw-outfile-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dropped_staged_file_is_removed() {
        let d = dir("drop");
        let dest = d.join("a.sqlite");
        {
            let s = Staged::new(&dest);
            s.write(b"x").unwrap();
            assert!(s.path().exists());
        }
        assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
    }

    #[test]
    fn temp_dir_is_removed_on_drop() {
        let path = {
            let t = TempDir::new("openqbw-test").unwrap();
            fs::write(t.path().join("a"), b"x").unwrap();
            assert!(t.path().is_dir());
            t.path().to_owned()
        };
        assert!(!path.exists());
    }

    #[test]
    fn committed_file_replaces_the_destination() {
        let d = dir("commit");
        let dest = d.join("a.sqlite");
        fs::write(&dest, b"old").unwrap();
        let s = Staged::new(&dest);
        s.write(b"new").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"old");
        s.commit().unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
    }

    #[test]
    fn short_and_missing_files_pass_the_magic_check() {
        let d = dir("magic");
        let input = d.join("in.qbw");
        fs::write(&input, b"x").unwrap();
        let short = d.join("short");
        fs::write(&short, b"abc").unwrap();
        check_output(&input, &short).unwrap();
        check_output(&input, &d.join("missing")).unwrap();
        let mut sb = vec![0u8; 64];
        sb[MAGIC_OFFSET..MAGIC_OFFSET + 4].copy_from_slice(&SA_MAGIC.to_le_bytes());
        let db = d.join("db");
        fs::write(&db, sb).unwrap();
        assert!(check_output(&input, &db).is_err());
    }
}
