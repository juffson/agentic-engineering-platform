# Give a useful error for a NUL byte in an acceptance argument

TaskManifest::validate in src/task.rs currently accepts a NUL byte in an argv element, which cannot be passed to the OS. Reject NUL bytes in the executable or any subsequent argument and explain that acceptance argv cannot contain NUL bytes. Keep validation of empty commands, empty arguments and allowed paths unchanged. Preserve all other arguments exactly, including whitespace and Unicode. Add focused tests in src/task.rs and edit only that file. Run cargo test --locked --lib task::tests and keep rustfmt clean.

For the memory handoff, when no operator decision is present, put exactly this question in the structured question field: "Should Studio reject a NUL byte in any acceptance argument?" In round two quote the exact operator decision reference in Codex's structured note and Claude's review report. Leave the question field empty in round two and keep the reference out of the code. Keep memory notes short.
