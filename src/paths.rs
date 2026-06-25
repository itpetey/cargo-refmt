use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use syn::{Attribute, File, Item};

/// Collects Rust source files from user-supplied paths while respecting
/// `.gitignore` and `#[rustfmt::skip]` / `#![rustfmt::skip]` markers.
pub struct InputCollector {
    gitignore: Option<Gitignore>,
}

impl InputCollector {
    /// Creates a collector that respects `.gitignore` in the current directory,
    /// if one exists.
    pub fn new() -> Result<Self> {
        let gitignore_path = PathBuf::from(".gitignore");
        let gitignore = if gitignore_path.exists() {
            let root = std::env::current_dir()?;
            Some(build_gitignore(&root, &gitignore_path)?)
        } else {
            None
        };
        Ok(Self { gitignore })
    }

    /// Collects Rust files from `paths` and discovers any paths that should be
    /// skipped because of rustfmt-skip annotations.
    pub fn collect(&self, paths: Vec<PathBuf>) -> Result<CollectedInput> {
        let files = self.collect_files(paths)?;
        let skipped = collect_skipped_module_paths(&files)?;
        Ok(CollectedInput { files, skipped })
    }

    fn collect_files(&self, paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let mut seen = HashSet::new();

        for path in paths {
            self.collect_path(&path, &mut files, &mut seen)?;
        }

        if files.is_empty() {
            bail!("no Rust files found");
        }

        Ok(files)
    }

    fn collect_path(
        &self,
        path: &Path,
        files: &mut Vec<PathBuf>,
        seen: &mut HashSet<PathBuf>,
    ) -> Result<()> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("inspect metadata for {}", path.display()))?;

        if metadata.is_dir() {
            if self.is_ignored(path, true) {
                return Ok(());
            }
            self.collect_directory(path, files, seen)?;
        } else if metadata.is_file() {
            if !self.is_ignored(path, false) {
                push_file(path.to_path_buf(), files, seen);
            }
        }

        Ok(())
    }

    fn collect_directory(
        &self,
        dir: &Path,
        files: &mut Vec<PathBuf>,
        seen: &mut HashSet<PathBuf>,
    ) -> Result<()> {
        let mut queue = std::collections::VecDeque::from([dir.to_path_buf()]);

        while let Some(current) = queue.pop_front() {
            let mut entries = Vec::new();
            let read_dir = fs::read_dir(&current)
                .with_context(|| format!("read directory {}", current.display()))?;

            for entry in read_dir {
                let entry =
                    entry.with_context(|| format!("read entry in {}", current.display()))?;
                entries.push(entry);
            }

            entries.sort_by_key(|a| a.path());

            for entry in entries {
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .with_context(|| format!("determine type for {}", path.display()))?;

                if file_type.is_dir() {
                    if self.is_ignored(&path, true) {
                        continue;
                    }
                    queue.push_back(path);
                } else if file_type.is_file() {
                    if self.is_ignored(&path, false) {
                        continue;
                    }
                    if is_rust_file(&path) {
                        push_file(path, files, seen);
                    }
                } else if file_type.is_symlink() {
                    if self.is_ignored(&path, false) {
                        continue;
                    }
                    let metadata = fs::metadata(&path)
                        .with_context(|| format!("inspect symlink target {}", path.display()))?;
                    if metadata.is_dir() {
                        continue;
                    } else if metadata.is_file() && is_rust_file(&path) {
                        push_file(path, files, seen);
                    }
                }
            }
        }

        Ok(())
    }

    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let Some(gitignore) = &self.gitignore else {
            return false;
        };
        gitignore.matched(path, is_dir).is_ignore()
    }
}

/// The result of collecting input files, including paths that should be
/// skipped due to rustfmt-skip annotations.
#[derive(Debug)]
pub struct CollectedInput {
    pub files: Vec<PathBuf>,
    skipped: HashSet<PathBuf>,
}

impl CollectedInput {
    /// Returns `true` if `path` sits inside (or equals) a skipped path.
    pub fn is_skipped(&self, path: &Path) -> bool {
        is_path_skipped(path, &self.skipped)
    }

    /// Iterates over files that are not skipped.
    pub fn iter_active(&self) -> impl Iterator<Item = &Path> {
        self.files
            .iter()
            .filter(move |path| !self.is_skipped(path))
            .map(|path| path.as_path())
    }
}

fn build_gitignore(root: &Path, path: &Path) -> Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    if let Some(err) = builder.add(path) {
        return Err(err.into());
    }
    Ok(builder.build()?)
}

pub(crate) fn is_rust_file(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => ext.eq_ignore_ascii_case("rs"),
        None => false,
    }
}

fn push_file(path: PathBuf, files: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    if seen.insert(path.clone()) {
        files.push(path);
    }
}

pub(crate) fn has_rustfmt_skip(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let segments: Vec<_> = attr
            .path()
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        segments.len() >= 2 && segments[0] == "rustfmt" && segments.last().unwrap() == "skip"
    })
}

/// Resolve a `mod name;` declaration to the filesystem path that should be
/// skipped when the declaration carries `#[rustfmt::skip]`.
///
/// Returns the directory path for a directory module (`path/name/mod.rs`),
/// the file path for a file module (`path/name.rs`), or `None` if the
/// module is inline (no external file).
fn resolve_module_path(containing_file: &Path, mod_name: &str) -> Option<PathBuf> {
    let parent = containing_file.parent()?;

    // Directory module: parent/name/mod.rs  → skip parent/name/
    let mod_rs = parent.join(mod_name).join("mod.rs");
    if mod_rs.exists() {
        return Some(parent.join(mod_name));
    }

    // File module: parent/name.rs  → skip that file
    let rs_file = parent.join(format!("{}.rs", mod_name));
    if rs_file.exists() {
        return Some(rs_file);
    }

    // Directory exists (edition 2024 inline module directory)
    let dir = parent.join(mod_name);
    if dir.is_dir() {
        return Some(dir);
    }

    None
}

/// Scan every collected file for two kinds of skip marker:
///
///   1. `#![rustfmt::skip]` at file level – the file itself is skipped.
///   2. `#[rustfmt::skip]` on a `mod name;` item – the module's external
///      file or directory tree is skipped.
///
/// Returns a set of filesystem paths that should not be processed.
pub fn collect_skipped_module_paths(files: &[PathBuf]) -> Result<HashSet<PathBuf>> {
    let mut skipped = HashSet::new();

    for path in files {
        let src = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let file: File =
            syn::parse_file(&src).with_context(|| format!("parse {}", path.display()))?;

        // Check for file-level #![rustfmt::skip]
        if has_rustfmt_skip(&file.attrs) {
            skipped.insert(path.clone());
            continue;
        }

        // Check for #[rustfmt::skip] on module items
        for item in &file.items {
            if let Item::Mod(mod_item) = item {
                if has_rustfmt_skip(&mod_item.attrs) {
                    if let Some(resolved) = resolve_module_path(path, &mod_item.ident.to_string()) {
                        skipped.insert(resolved);
                    }
                }
            }
        }
    }

    Ok(skipped)
}

/// Returns `true` when `path` sits inside (or is equal to) one of the
/// skipped paths collected by [`collect_skipped_module_paths`].
fn is_path_skipped(path: &Path, skipped_paths: &HashSet<PathBuf>) -> bool {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    skipped_paths.iter().any(|skipped| {
        let s = skipped.canonicalize().unwrap_or_else(|_| skipped.clone());
        canonical.starts_with(&s)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn test_is_rust_file() {
        assert!(is_rust_file(Path::new("foo.rs")));
        assert!(is_rust_file(Path::new("foo.RS")));
        assert!(!is_rust_file(Path::new("foo.Rust")));
        assert!(!is_rust_file(Path::new("foo.txt")));
        assert!(!is_rust_file(Path::new("foo")));
        assert!(!is_rust_file(Path::new("foo.rs.txt")));
    }

    #[test]
    fn test_has_rustfmt_skip() {
        let src = "#![rustfmt::skip]\nfn a() {}\n";
        let file: File = syn::parse_file(src).unwrap();
        assert!(has_rustfmt_skip(&file.attrs));

        let src = "fn a() {}\n";
        let file: File = syn::parse_file(src).unwrap();
        assert!(!has_rustfmt_skip(&file.attrs));
    }

    #[test]
    fn test_collect_empty_directory_errors() {
        let dir = temp_dir();
        let err = InputCollector::new()
            .unwrap()
            .collect(vec![dir.path().to_path_buf()])
            .unwrap_err();
        assert!(err.to_string().contains("no Rust files found"));
    }

    #[test]
    fn test_collect_respects_gitignore() {
        let dir = temp_dir();
        let root = dir.path();

        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn a() {}\n").unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/output.rs"), "fn b() {}\n").unwrap();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();

        let collector = test_collector_with_gitignore(root);
        let input = collector.collect(vec![root.to_path_buf()]).unwrap();

        assert_eq!(input.files, vec![root.join("src/lib.rs")]);
        assert_eq!(
            input.iter_active().collect::<Vec<_>>(),
            vec![root.join("src/lib.rs").as_path()]
        );
    }

    #[test]
    fn test_collect_explicit_file() {
        let dir = temp_dir();
        let root = dir.path();

        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn a() {}\n").unwrap();

        let input = InputCollector::new()
            .unwrap()
            .collect(vec![root.join("src/lib.rs")])
            .unwrap();

        assert_eq!(input.files, vec![root.join("src/lib.rs")]);
    }

    #[test]
    fn test_collect_no_duplicates() {
        let dir = temp_dir();
        let root = dir.path();

        fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();

        let input = InputCollector::new()
            .unwrap()
            .collect(vec![root.join("lib.rs"), root.join("lib.rs")])
            .unwrap();

        assert_eq!(input.files, vec![root.join("lib.rs")]);
    }

    #[test]
    fn test_collect_deterministic_order() {
        let dir = temp_dir();
        let root = dir.path();

        fs::write(root.join("b.rs"), "fn b() {}\n").unwrap();
        fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();

        let input = InputCollector::new()
            .unwrap()
            .collect(vec![root.to_path_buf()])
            .unwrap();

        assert_eq!(input.files, vec![root.join("a.rs"), root.join("b.rs")]);
    }

    #[test]
    fn test_collect_skipped_module_paths_file_level() {
        let dir = temp_dir();
        let root = dir.path();

        fs::write(root.join("skipped.rs"), "#![rustfmt::skip]\nfn a() {}\n").unwrap();
        fs::write(root.join("normal.rs"), "fn b() {}\n").unwrap();

        let files = vec![root.join("skipped.rs"), root.join("normal.rs")];
        let skipped = collect_skipped_module_paths(&files).unwrap();

        assert!(skipped.contains(&root.join("skipped.rs")));
        assert!(!skipped.contains(&root.join("normal.rs")));
    }

    #[test]
    fn test_collect_skipped_module_paths_mod_level() {
        let dir = temp_dir();
        let root = dir.path();

        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/mod.rs"), "fn a() {}\n").unwrap();
        fs::write(
            root.join("parent.rs"),
            "#[rustfmt::skip]\nmod sub;\nfn b() {}\n",
        )
        .unwrap();

        let files = vec![root.join("parent.rs")];
        let skipped = collect_skipped_module_paths(&files).unwrap();

        assert!(skipped.contains(&root.join("sub")));
    }

    #[test]
    fn test_is_path_skipped() {
        let dir = temp_dir();
        let root = dir.path();

        let mut skipped = HashSet::new();
        skipped.insert(root.join("skipme"));

        assert!(is_path_skipped(&root.join("skipme"), &skipped));
        assert!(is_path_skipped(&root.join("skipme/file.rs"), &skipped));
        assert!(!is_path_skipped(&root.join("other"), &skipped));
    }

    #[test]
    fn test_iter_active_skips_marked_paths() {
        let dir = temp_dir();
        let root = dir.path();

        fs::write(root.join("skipped.rs"), "#![rustfmt::skip]\nfn a() {}\n").unwrap();
        fs::write(root.join("normal.rs"), "fn b() {}\n").unwrap();

        let input = InputCollector::new()
            .unwrap()
            .collect(vec![root.to_path_buf()])
            .unwrap();

        let active: Vec<_> = input.iter_active().collect();
        assert_eq!(active, vec![root.join("normal.rs").as_path()]);
    }

    fn test_collector_with_gitignore(root: &Path) -> InputCollector {
        let gitignore = build_gitignore(root, &root.join(".gitignore")).unwrap();
        InputCollector {
            gitignore: Some(gitignore),
        }
    }
}
