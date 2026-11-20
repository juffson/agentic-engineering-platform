# Reject NUL bytes in AEP manifest paths

AEP's task manifest currently allows a NUL byte in an allowed file path. Git and the operating system cannot represent that filename. Reject this at manifest validation so invalid tasks are refused before execution.

- AC-1: Reject allowed_paths entries containing a NUL byte at the beginning, inside a path component, or at the end.
- AC-2: Continue accepting valid nested paths, spaces and UTF-8 in filenames. Keep the existing validation for empty, traversal and Git metadata path components.
- Edit only src/task.rs. The pinned acceptance test calls the public TaskManifest validation API.
