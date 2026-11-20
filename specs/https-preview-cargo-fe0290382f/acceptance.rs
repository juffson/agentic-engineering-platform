use aep::studio::{Artifact, preview};
use std::{fs, path::PathBuf};

fn show(root: &PathBuf, bytes: &[u8]) -> aep::studio::Preview {
    fs::write(root.join("runs/pilot/checks.log"), bytes).unwrap();
    preview(root, "pilot", Artifact::Checks).unwrap().unwrap()
}

#[test]
fn verifies_the_preview_at_utf8_boundaries() {
    const LIMIT: usize = 48 * 1024;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    fs::create_dir_all(root.join("runs/pilot")).unwrap();
    let mut bytes = "雪雪".as_bytes().to_vec();
    bytes.extend(std::iter::repeat_n(b'b', LIMIT - 1));
    let cut = show(&root, &bytes);
    assert!(cut.truncated);
    assert_eq!(cut.text.as_bytes(), &[b'b'; LIMIT - 1]);
    let mut aligned = b"prefix".to_vec();
    aligned.extend("雪".as_bytes());
    aligned.extend(std::iter::repeat_n(b'a', LIMIT - 3));
    let full = show(&root, &aligned);
    assert!(full.truncated);
    assert!(full.text.starts_with("雪a"));
    assert_eq!(full.text.len(), LIMIT);
    let short = show(&root, b"a\xffz");
    assert!(!short.truncated);
    assert_eq!(short.text, "a\u{fffd}z");
    let mut malformed_tail = vec![b'x'; LIMIT + 3];
    malformed_tail[3] = 0x80;
    assert!(show(&root, &malformed_tail).text.starts_with('\u{fffd}'));
}
