# Keep truncated Studio logs aligned to UTF-8

A preview of src/studio.rs starts at the last 48 KiB. If the file is truncated, skip only the leading UTF-8 continuation bytes. Keep lossy decoding for malformed bytes later, the limit, and the path safety checks. Add focused tests in src/studio_tests.rs. Edit only those two files. Run cargo test --locked --lib studio::tests and keep rustfmt clean.

For the memory handoff, when no operator decision is present, put this question in the structured question field: "Should a truncated preview omit the partial leading character?" In round two quote the exact decision reference in Codex's note and Claude's review report. Leave the question field empty in round two and keep the reference out of the code. Keep memory notes short.
