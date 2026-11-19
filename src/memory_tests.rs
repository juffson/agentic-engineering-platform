use super::*;
use std::thread;

#[test]
fn scoped_context_revisions_and_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = Memory::open(tmp.path()).unwrap();
    let project = Key::new("", "", "");
    let task = Key::new("task", "spec", "");
    let code = Key::new("task", "spec", "code");
    let old = db.add(&project, "decision", "old", None).unwrap();
    let new = db
        .add(&project, "decision", "corrected", Some(old))
        .unwrap();
    assert!(db.add(&task, "decision", "wrong scope", Some(new)).is_err());
    assert!(
        db.add(&project, "decision", "old version", Some(old))
            .is_err()
    );
    assert_eq!(db.get(old).unwrap()["body"], "old");
    let payload = json!({"note":"implementation","question":"which time?"});
    let ids = db.publish("implementer", "run", &code, &payload).unwrap();
    assert_eq!(
        db.publish("implementer", "run", &code, &payload).unwrap(),
        ids
    );
    assert!(
        db.publish(
            "implementer",
            "run",
            &code,
            &json!({"note":"other","question":"which time?"})
        )
        .is_err()
    );
    assert!(
        db.publish(
            "reviewer",
            "run",
            &code,
            &json!({"note":"review","question":"x".repeat(1001)})
        )
        .is_err()
    );
    let selected = |db: &Memory, k: &Key| {
        db.context(k, None)
            .unwrap()
            .into_iter()
            .map(|v| v["id"].as_i64().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(selected(&db, &code), vec![ids[0], ids[1], new]);
    assert_eq!(selected(&db, &task), vec![ids[1], new]);
    assert_eq!(selected(&db, &Key::new("task", "other", "code")), vec![new]);
    assert_eq!(selected(&db, &Key::new("other", "spec", "code")), vec![new]);
    for _ in 0..20 {
        db.add(&project, "decision", &"量".repeat(800), None)
            .unwrap();
    }
    let list = db.context(&code, Some("run")).unwrap();
    assert!(list.len() <= 12 && serde_json::to_vec(&list).unwrap().len() <= 8192);
    assert_eq!(list[0]["run_id"], "run");
    let required = db
        .context_with_required(&code, Some("run"), Some(new))
        .unwrap();
    assert_eq!(required[0]["id"], new);
    assert!(required.len() <= 12 && serde_json::to_vec(&required).unwrap().len() <= 8192);
    assert!(db.context_with_required(&code, None, Some(old)).is_err());
    assert!(
        db.context_with_required(&Key::new("task", "other", "code"), None, Some(ids[0]))
            .is_err()
    );
}
#[test]
fn concurrent_updates_share_one_successor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = Memory::open(tmp.path()).unwrap();
    let old = db
        .add(&Key::new("", "", ""), "decision", "original", None)
        .unwrap();
    let mut threads = vec![];
    for i in 0..8 {
        let path = tmp.path().to_owned();
        threads.push(thread::spawn(move || {
            let mut db = Memory::open(&path).unwrap();
            let key = Key::new("", "", "");
            db.add(&key, "decision", &format!("independent {i}"), None)
                .unwrap();
            db.add(&key, "decision", &format!("revision {i}"), Some(old))
                .is_ok()
        }));
    }
    assert_eq!(
        threads
            .into_iter()
            .map(|j| j.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
    assert_eq!(db.context(&Key::new("", "", ""), None).unwrap().len(), 9);
}

#[test]
fn manual_revision_of_an_agent_question_keeps_its_scope_and_provenance_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = Memory::open(tmp.path()).unwrap();
    let k = Key::new("task", "spec", "candidate");
    let notes = db
        .publish(
            "reviewer",
            "first-run",
            &k,
            &json!({"note":"review","question":"Which unit?"}),
        )
        .unwrap();
    let old = notes[1];
    let revised = db.revise(old, Kind::Decision, "Milliseconds").unwrap();
    assert!(
        db.revise(old, Kind::Decision, "Stale answer")
            .unwrap_err()
            .is::<RevisionConflict>()
    );
    let original = db.get(old).unwrap();
    let new = db.get(revised).unwrap();
    assert_eq!(original["run_id"], "first-run");
    assert_eq!(original["author"], "reviewer");
    assert_eq!(original["body"], "Which unit?");
    assert_eq!(new["author"], "manual");
    assert_eq!(new["replaces"], old);
    assert_eq!(new["kind"], "decision");
    assert!(new["run_id"].is_null());
    for field in ["task", "spec_sha", "scope", "candidate_sha"] {
        assert_eq!(new[field], original[field]);
    }
    let next = db
        .context(
            &Key::new("task", "spec", "other-candidate"),
            Some("next-run"),
        )
        .unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0]["id"], revised);
    assert!(
        db.context(&Key::new("task", "other-spec", ""), None)
            .unwrap()
            .is_empty()
    );
}
