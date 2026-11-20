# Preserve valid UTF-8 at the start of truncated Studio log previews

Studio reads at most the last 48 KiB of a Run log. When the tail offset lands inside a valid UTF-8 character, the preview should omit that partial leading character so an otherwise valid log does not start with a replacement character. Change only src/studio.rs. Keep the existing bounds, artifact safety, and lossy decoding of independent malformed bytes.

- AC-1: a valid character cut at the first byte boundary of the selected tail is omitted, the remainder is unchanged, and truncated remains true.
- AC-2: a leading malformed byte in an untruncated log is still represented by U+FFFD, and plain aligned tails are unchanged.

The pinned acceptance fixture tests both cases. Run the repository Studio preview tests when convenient. Both input logs are synthetic.
