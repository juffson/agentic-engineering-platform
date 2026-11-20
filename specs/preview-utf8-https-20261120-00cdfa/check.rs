use aep::studio::{Artifact, Preview, preview};
use std::fs;

const LIMIT: usize = 48 * 1024;

fn log(body: &[u8]) -> Preview {
    let data = tempfile::tempdir().unwrap();
    let dir = data.path().join("runs/pilot");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("checks.log"), body).unwrap();
    preview(data.path(), "pilot", Artifact::Checks)
        .unwrap()
        .unwrap()
}

#[test]
fn truncated_tail_omits_a_cut_utf8_character() {
    for omitted in [1, 2] {
        let suffix = "a".repeat(LIMIT - omitted);
        let mut body = b"prefix".to_vec();
        body.extend("阅".as_bytes());
        body.extend(suffix.as_bytes());
        let result = log(&body);
        assert!(result.truncated);
        assert_eq!(result.text, suffix);
    }
}

#[test]
fn preserves_aligned_tails_and_untruncated_malformed_input() {
    let aligned = format!("prefix{}", "Z".repeat(LIMIT));
    let result = log(aligned.as_bytes());
    assert!(result.truncated);
    assert_eq!(result.text, "Z".repeat(LIMIT));
    let result = log(b"\x80valid");
    assert!(!result.truncated);
    assert_eq!(result.text, "\u{fffd}valid");
}

#[test]
fn preserves_a_standalone_malformed_byte_at_a_truncated_boundary() {
    let mut body = b"prefix\x80".to_vec();
    let suffix = "y".repeat(LIMIT - 1);
    body.extend(suffix.as_bytes());
    let result = log(&body);
    assert!(result.truncated);
    assert_eq!(result.text, format!("\u{fffd}{suffix}"));
}
