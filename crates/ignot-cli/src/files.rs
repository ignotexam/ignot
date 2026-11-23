//! An exam on disk.
//!
//! One exam is one folder: `manifest.json` and `exam.age` from the moment it is sealed; `exam.json`,
//! `answers/` and `results.json` once it has opened. While models are being asked, `run.partial.jsonl` holds
//! the answers that are in; it is gone when they are saved.

use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ignot_core::results::AnswerFile;
use serde::Serialize;

pub struct Paths {
    pub manifest: PathBuf,
    pub sealed: PathBuf,
    pub exam: PathBuf,
    pub answers: PathBuf,
    pub results: PathBuf,
    /// The answers of a run that is under way, or that was cut short.
    pub journal: PathBuf,
    /// The file a run locks while it is under way.
    pub lock: PathBuf,
}

pub fn paths(dir: &Path) -> Paths {
    Paths {
        manifest: dir.join("manifest.json"),
        sealed: dir.join("exam.age"),
        exam: dir.join("exam.json"),
        answers: dir.join("answers"),
        results: dir.join("results.json"),
        journal: dir.join("run.partial.jsonl"),
        lock: dir.join("run.partial.lock"),
    }
}

/// The files of an exam, exactly as they are on disk.
pub struct ExamFiles {
    pub manifest: String,
    pub sealed: Vec<u8>,
    pub exam: Option<String>,
    pub answers: Option<Vec<String>>,
    pub results: Option<String>,
}

pub fn read_text(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))
}

fn read_if_there(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

pub fn load(dir: &Path) -> Result<ExamFiles> {
    let at = paths(dir);
    let sealed = fs::read(&at.sealed).with_context(|| format!("cannot read {}", at.sealed.display()))?;
    let answers = match fs::read_dir(&at.answers) {
        Ok(entries) => {
            let mut files = Vec::new();
            for entry in entries {
                let path = entry.with_context(|| format!("cannot list {}", at.answers.display()))?.path();
                if path.extension().is_some_and(|extension| extension == "json") {
                    files.push(path);
                }
            }
            files.sort();
            Some(files.iter().map(|path| read_text(path)).collect::<Result<Vec<_>>>()?)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("cannot list {}", at.answers.display())),
    };
    Ok(ExamFiles {
        manifest: read_text(&at.manifest)?,
        sealed,
        exam: read_if_there(&at.exam)?,
        answers,
        results: read_if_there(&at.results)?,
    })
}

/// Writes a file that must not exist yet: nothing a command writes this way ever replaces something.
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    create(path, false)?.write_all(bytes).with_context(|| format!("cannot write {}", path.display()))
}

/// Like [`write_new`], for a file only its owner may read.
pub fn write_new_private(path: &Path, bytes: &[u8]) -> Result<()> {
    create(path, true)?.write_all(bytes).with_context(|| format!("cannot write {}", path.display()))
}

fn create(path: &Path, private: bool) -> Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    match options.open(path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            bail!("{} already exists; move it away first", path.display())
        }
        Err(e) => Err(e).with_context(|| format!("cannot create {}", path.display())),
    }
}

/// Replaces a file in one step, so a crash never leaves half of it. The new text is first put on the disk where
/// the volume can do that, so a power cut does not leave an empty file in its place.
pub fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let write = || -> io::Result<()> {
        let mut file = fs::File::create(&partial)?;
        file.write_all(bytes)?;
        // A volume that cannot sync, as some network mounts cannot, still has the text.
        file.sync_all().ok();
        fs::rename(&partial, path)
    };
    write().with_context(|| format!("cannot write {}", path.display()))
}

/// The whole lines of a run's journal, when there is one. A run that was cut short may have stopped in the
/// middle of writing a line, even in the middle of a character, so whatever follows the last line break is
/// left out.
pub fn read_journal(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(mut bytes) => {
            bytes.truncate(bytes.iter().rposition(|&byte| byte == b'\n').map_or(0, |end| end + 1));
            String::from_utf8(bytes).map(Some).with_context(|| format!("{} is not text", path.display()))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// A file opened to add to, whose every flush goes through to the disk where the volume can do that: an answer
/// that was written survives a power cut, not only a crash.
pub struct Durable(fs::File);

impl Durable {
    pub fn append(path: &Path) -> Result<Self> {
        let file =
            OpenOptions::new().append(true).open(path).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(Self(file))
    }
}

impl Write for Durable {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        // A volume that cannot sync still has the line: it is kept, not refused.
        self.0.sync_data().ok();
        Ok(())
    }
}

/// JSON the way it is kept on disk: indented, with a final newline.
pub fn pretty<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)? + "\n")
}

/// A model's name as a file name.
pub fn slug(model: &str) -> String {
    let mut out = String::new();
    for c in model.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out
}

/// Refuses a set of models in which two different names would be saved under one file name.
pub fn distinct_names(models: &[&str]) -> Result<()> {
    let mut names: Vec<(String, &str)> = models.iter().map(|model| (slug(model), *model)).collect();
    names.sort();
    names.dedup();
    if let Some(pair) = names.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        bail!("{} and {} would both be saved as {}.json", pair[0].1, pair[1].1, pair[0].0);
    }
    Ok(())
}

/// Writes one file per model into the exam's `answers` folder.
pub fn save_answers(dir: &Path, files: &[AnswerFile]) -> Result<()> {
    let folder = paths(dir).answers;
    distinct_names(&files.iter().map(|file| file.model.as_str()).collect::<Vec<_>>())?;
    fs::create_dir_all(&folder).with_context(|| format!("cannot create {}", folder.display()))?;
    for file in files {
        replace(&folder.join(format!("{}.json", slug(&file.model))), pretty(file)?.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A folder of its own for one test, removed when the test ends.
    pub struct Scratch(pub PathBuf);

    impl Scratch {
        pub fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let name = format!("ignot-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst));
            let path = std::env::temp_dir().join(name);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_new_file_never_replaces_one() {
        let scratch = Scratch::new();
        let path = scratch.path("a.txt");
        write_new(&path, b"first").unwrap();
        let error = write_new(&path, b"second").unwrap_err().to_string();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert!(write_new_private(&path, b"third").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let path = scratch.path("keys.json");
        write_new_private(&path, b"{}").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn replacing_leaves_no_partial_file() {
        let scratch = Scratch::new();
        let path = scratch.path("results.json");
        replace(&path, b"one").unwrap();
        replace(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
    }

    #[test]
    fn a_journal_cut_anywhere_reads_up_to_its_last_whole_line() {
        let scratch = Scratch::new();
        let path = scratch.path("run.partial.jsonl");
        assert!(read_journal(&path).unwrap().is_none());
        let text = "{\"a\":\"caf\u{e9}\"}\n{\"b\":\"\u{4e2d}\u{6587}\"}\n";
        for cut in 0..=text.len() {
            fs::write(&path, &text.as_bytes()[..cut]).unwrap();
            let whole = read_journal(&path).unwrap().unwrap();
            let expected = text.as_bytes()[..cut].iter().rposition(|&byte| byte == b'\n').map_or(0, |end| end + 1);
            assert_eq!(whole.len(), expected, "cut at {cut}");
        }
        let mut journal = Durable::append(&path).unwrap();
        writeln!(journal, "more").unwrap();
        journal.flush().unwrap();
        assert!(fs::read_to_string(&path).unwrap().ends_with("more\n"));
    }

    #[test]
    fn model_names_become_file_names() {
        assert_eq!(slug("anthropic/claude-haiku-5.5"), "anthropic_claude-haiku-5.5");
        assert_eq!(slug("lab/model:free"), "lab_model_free");
        assert_eq!(slug("a//b  c"), "a_b_c");
        assert_eq!(slug("../../etc/passwd"), ".._.._etc_passwd");
    }

    #[test]
    fn a_folder_loads_with_what_is_there() {
        let scratch = Scratch::new();
        assert!(load(&scratch.0).is_err(), "no sealed file");
        fs::write(scratch.path("exam.age"), [0xff, 0x00]).unwrap();
        assert!(load(&scratch.0).is_err(), "no manifest");
        fs::write(scratch.path("manifest.json"), "{}").unwrap();
        let files = load(&scratch.0).unwrap();
        assert_eq!((files.manifest.as_str(), files.sealed.as_slice()), ("{}", &[0xff, 0x00][..]));
        assert!(files.exam.is_none() && files.answers.is_none() && files.results.is_none());
        fs::create_dir(scratch.path("answers")).unwrap();
        fs::write(scratch.path("answers/b.json"), "B").unwrap();
        fs::write(scratch.path("answers/a.json"), "A").unwrap();
        fs::write(scratch.path("answers/notes.txt"), "not an answer file").unwrap();
        fs::write(scratch.path("exam.json"), "E").unwrap();
        fs::write(scratch.path("results.json"), "R").unwrap();
        let files = load(&scratch.0).unwrap();
        assert_eq!(files.answers, Some(vec!["A".to_string(), "B".to_string()]));
        assert_eq!((files.exam.as_deref(), files.results.as_deref()), (Some("E"), Some("R")));
    }
}
