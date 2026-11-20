use aep::task::TaskManifest;

fn accepts(path: &str) -> bool {
    TaskManifest {
        title: None,
        allowed_paths: vec![path.into()],
        check: vec!["true".into()],
        criteria: Vec::new(),
        checks: Vec::new(),
    }
    .validate()
    .is_ok()
}

#[test]
fn validates_real_git_paths_at_dispatch() {
    for path in ["\0src/task.rs", "src/ta\0sk.rs", "src/task.rs\0", "src/\0/nested.rs"] {
        assert!(!accepts(path), "accepted {path:?}");
    }
    for path in ["src/task.rs", "src/nested/a b.rs", "说明/中文.md"] {
        assert!(accepts(path), "rejected {path:?}");
    }
    for path in ["", "/src/task.rs", "src//task.rs", "src/../task.rs", "src/.git/config"] {
        assert!(!accepts(path), "accepted {path:?}");
    }
}
