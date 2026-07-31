// SandboxPath is constructable only via validation that the resolved
// path is contained within a given root directory (the directory
// emed-code was launched from, once tool dispatch wires this in) —
// containment is enforced by the type system, not a convention every
// call site has to remember. Tool output feeds back into the LLM
// conversation, making an escaped path a real prompt-injection-adjacent
// risk, more so than in a typical read-only tool.

use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub(crate) enum SandboxError {
    NotFound,
    Escapes,
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SandboxError::NotFound => write!(f, "path does not exist or could not be resolved"),
            SandboxError::Escapes => write!(f, "path escapes the sandboxed directory"),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct SandboxPath {
    absolute: PathBuf,
    // Canonicalized (symlink-resolved) and relative to root — kept
    // alongside `absolute` specifically so the content-sensitivity
    // blocklist (tools.rs) can match against it instead of the raw
    // requested string, catching a symlink with an innocuous name that
    // resolves to a blocked file.
    relative: PathBuf,
}

impl SandboxPath {
    // Canonicalizes both root and the joined candidate path itself
    // (rather than trusting the caller to have already canonicalized
    // root) — cheap per call, and avoids a subtle correctness trap if a
    // caller ever passes a root that isn't already canonical.
    pub(crate) fn new(root: &Path, requested: &Path) -> Result<Self, SandboxError> {
        let canonical_root = root.canonicalize().map_err(|_| SandboxError::NotFound)?;
        let candidate = canonical_root.join(requested);
        let canonical = candidate
            .canonicalize()
            .map_err(|_| SandboxError::NotFound)?;

        if !canonical.starts_with(&canonical_root) {
            return Err(SandboxError::Escapes);
        }

        let relative = canonical
            .strip_prefix(&canonical_root)
            .expect("just checked canonical starts_with canonical_root")
            .to_path_buf();

        Ok(SandboxPath {
            absolute: canonical,
            relative,
        })
    }

    // For write_file: the target may not exist yet (creating a new
    // file), so only the *parent* directory needs to already exist and
    // be canonicalized/contained — no mkdir -p, the parent must be real.
    // If the target itself already exists (the overwrite case), it's
    // still canonicalized in full and re-checked for containment, so a
    // symlink sitting at that name pointing outside the sandbox is
    // caught, same guarantee `new` gives reads.
    pub(crate) fn new_for_write(root: &Path, requested: &Path) -> Result<Self, SandboxError> {
        let canonical_root = root.canonicalize().map_err(|_| SandboxError::NotFound)?;

        // file_name() is None for a path with no normal final component
        // (e.g. ".", ".."), which can't be a write target either way —
        // folded into NotFound rather than a new variant, since it's the
        // same "nothing to identify here" condition.
        let file_name = requested.file_name().ok_or(SandboxError::NotFound)?;
        let parent = requested.parent().unwrap_or_else(|| Path::new(""));

        let candidate_parent = canonical_root.join(parent);
        let canonical_parent = candidate_parent
            .canonicalize()
            .map_err(|_| SandboxError::NotFound)?;

        if !canonical_parent.starts_with(&canonical_root) {
            return Err(SandboxError::Escapes);
        }

        let candidate = canonical_parent.join(file_name);
        // If the target already exists, canonicalizing it in full
        // resolves a symlink at that exact name; if it doesn't exist
        // yet, canonicalize simply fails and the already-contained
        // (parent-only-canonicalized) candidate is used as-is — safe,
        // since file_name() above guarantees no ".."/"." component to
        // exploit.
        let canonical = candidate.canonicalize().unwrap_or(candidate);

        if !canonical.starts_with(&canonical_root) {
            return Err(SandboxError::Escapes);
        }

        let relative = canonical
            .strip_prefix(&canonical_root)
            .expect("just checked canonical starts_with canonical_root")
            .to_path_buf();

        Ok(SandboxPath {
            absolute: canonical,
            relative,
        })
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.absolute
    }

    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    // Hand-rolled instead of adding a tempfile dev-dependency — a small
    // enough amount of code to be worth writing directly, same reasoning
    // as ChatError over thiserror. Drop guarantees cleanup even if a
    // test panics partway through.
    struct TempDir(PathBuf);

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    impl TempDir {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "emed-code-sandbox-path-test-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sandbox_path_accepts_a_path_contained_within_the_root() {
        let root = TempDir::new();
        std::fs::write(root.path().join("notes.txt"), "hello").unwrap();

        let result = SandboxPath::new(root.path(), Path::new("notes.txt"));

        assert!(result.is_ok());
    }

    #[test]
    fn sandbox_path_rejects_a_path_escaping_via_dotdot() {
        let outer = TempDir::new();
        let root = outer.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(outer.path().join("outside.txt"), "secret").unwrap();

        let result = SandboxPath::new(&root, Path::new("../outside.txt"));

        assert_eq!(result, Err(SandboxError::Escapes));
    }

    #[test]
    fn sandbox_path_rejects_an_absolute_path_outside_the_root() {
        let outer = TempDir::new();
        let root = outer.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let outside_file = outer.path().join("outside.txt");
        std::fs::write(&outside_file, "secret").unwrap();

        let result = SandboxPath::new(&root, &outside_file);

        assert_eq!(result, Err(SandboxError::Escapes));
    }

    #[test]
    #[cfg(unix)]
    fn sandbox_path_rejects_a_symlink_escaping_the_root() {
        let outer = TempDir::new();
        let root = outer.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let outside_file = outer.path().join("outside.txt");
        std::fs::write(&outside_file, "secret").unwrap();
        std::os::unix::fs::symlink(&outside_file, root.join("link")).unwrap();

        // The literal path "link" looks contained — only resolving the
        // symlink (via canonicalize) reveals it points outside root.
        let result = SandboxPath::new(&root, Path::new("link"));

        assert_eq!(result, Err(SandboxError::Escapes));
    }

    // The canonicalized, symlink-resolved path relative to root — this
    // is what the content-sensitivity blocklist (tools.rs) checks,
    // specifically so a symlink with an innocuous name pointing at a
    // blocked file (e.g. ".env") can't bypass a check against the
    // literal requested string.
    #[test]
    fn sandbox_path_relative_path_returns_the_path_relative_to_root() {
        let root = TempDir::new();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        std::fs::write(root.path().join("sub").join("file.txt"), "hi").unwrap();

        let sandbox_path = SandboxPath::new(root.path(), Path::new("sub/file.txt")).unwrap();

        assert_eq!(sandbox_path.relative_path(), Path::new("sub/file.txt"));
    }

    #[test]
    fn sandbox_path_rejects_a_path_that_does_not_exist() {
        let root = TempDir::new();

        let result = SandboxPath::new(root.path(), Path::new("nope.txt"));

        assert_eq!(result, Err(SandboxError::NotFound));
    }

    // Phase 4 Step 1: a write-oriented constructor, for a target that
    // may not exist yet (write_file can create a new file, not just
    // overwrite one). Unlike SandboxPath::new, only the *parent*
    // directory needs to already exist.
    #[test]
    fn sandbox_path_new_for_write_accepts_a_new_file_in_an_existing_directory() {
        let root = TempDir::new();

        let result = SandboxPath::new_for_write(root.path(), Path::new("new.txt"));

        assert!(result.is_ok());
    }

    #[test]
    fn sandbox_path_new_for_write_accepts_overwriting_an_existing_file() {
        let root = TempDir::new();
        std::fs::write(root.path().join("existing.txt"), "old content").unwrap();

        let result = SandboxPath::new_for_write(root.path(), Path::new("existing.txt"));

        assert!(result.is_ok());
    }

    #[test]
    fn sandbox_path_new_for_write_rejects_a_missing_parent_directory() {
        let root = TempDir::new();

        let result = SandboxPath::new_for_write(root.path(), Path::new("no_such_dir/new.txt"));

        assert_eq!(result, Err(SandboxError::NotFound));
    }

    #[test]
    #[cfg(unix)]
    fn sandbox_path_new_for_write_rejects_a_parent_directory_escaping_via_symlink() {
        let outer = TempDir::new();
        let root = outer.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let outside_dir = outer.path().join("outside_dir");
        std::fs::create_dir(&outside_dir).unwrap();
        std::os::unix::fs::symlink(&outside_dir, root.join("link_dir")).unwrap();

        let result = SandboxPath::new_for_write(&root, Path::new("link_dir/new.txt"));

        assert_eq!(result, Err(SandboxError::Escapes));
    }

    // The concrete case that justifies still canonicalizing the full
    // path when the target already exists: an overwrite must not be
    // allowed to write through a symlink pointing outside the sandbox,
    // the same guarantee SandboxPath::new already gives reads.
    #[test]
    #[cfg(unix)]
    fn sandbox_path_new_for_write_rejects_overwriting_a_symlink_that_escapes_the_sandbox() {
        let outer = TempDir::new();
        let root = outer.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let secret = outer.path().join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        std::os::unix::fs::symlink(&secret, root.join("link.txt")).unwrap();

        let result = SandboxPath::new_for_write(&root, Path::new("link.txt"));

        assert_eq!(result, Err(SandboxError::Escapes));
    }

    #[test]
    fn sandbox_path_new_for_write_relative_path_is_relative_to_root() {
        let root = TempDir::new();
        std::fs::create_dir(root.path().join("sub")).unwrap();

        let sandbox_path =
            SandboxPath::new_for_write(root.path(), Path::new("sub/new.txt")).unwrap();

        assert_eq!(sandbox_path.relative_path(), Path::new("sub/new.txt"));
    }

    // Review focus for this step: a rejected symlink's error must not
    // reveal the real target it resolved to. Each SandboxError variant's
    // Display is a fixed string, never interpolating any path — this
    // pins that down structurally, not just by inspection.
    #[test]
    fn sandbox_error_display_never_contains_any_path_fragment() {
        assert_eq!(
            SandboxError::Escapes.to_string(),
            "path escapes the sandboxed directory"
        );
        assert_eq!(
            SandboxError::NotFound.to_string(),
            "path does not exist or could not be resolved"
        );
    }
}
