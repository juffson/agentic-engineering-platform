# Reject NUL characters in task allowed paths

TaskManifest currently accepts file paths containing a NUL character, even though the underlying Git and filesystem APIs cannot represent them. Fix TaskManifest::validate in src/task.rs only and retain valid Unicode and space-containing paths.

## Acceptance

- AC-1 Reject an allowed path containing NUL in any component using the existing invalid allowed_paths error.
- AC-2 Keep ordinary relative paths, including Unicode and spaces, valid and preserve rejection of empty, absolute, parent-traversal, and .git path components.
