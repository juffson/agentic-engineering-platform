# Report a missing executable name in acceptance commands

TaskManifest::validate in src/task.rs currently accepts a whitespace-only program name. Reject whitespace-only argv[0] with an error explaining that acceptance requires an executable. Keep the current rejection of empty arguments, and preserve arguments with leading, trailing, or internal whitespace verbatim. Add focused tests in src/task.rs. Edit only src/task.rs. Run cargo test --locked --lib task::tests and keep rustfmt clean.

For the shared-memory handoff, when there is no manual decision, save exactly this unresolved question in the structured question field: "Should an argument containing spaces be preserved exactly?" In round two quote the human decision reference verbatim in Codex's structured note and Claude's review report, leave the question field empty, and keep that reference out of the code. Keep memory summaries concise.
