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
pub(crate) struct SandboxPath(PathBuf);

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

        if canonical.starts_with(&canonical_root) {
            Ok(SandboxPath(canonical))
        } else {
            Err(SandboxError::Escapes)
        }
    }

    pub(crate) fn as_path(&self) -> &Path {
        &self.0
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

    #[test]
    fn sandbox_path_rejects_a_path_that_does_not_exist() {
        let root = TempDir::new();

        let result = SandboxPath::new(root.path(), Path::new("nope.txt"));

        assert_eq!(result, Err(SandboxError::NotFound));
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
