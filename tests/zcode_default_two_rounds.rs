#[path = "support/zcode_two_rounds.rs"]
mod support;

use aep::project::Library;
use anyhow::Result;
use serde_json::json;
use std::fs;
use support::{Fixture, MODEL_A, MODEL_B};

// T1: AC-1 and the project-isolation portion of AC-2. Studio prefill is T3.
#[test]
fn default_save_read_and_clear_persist_across_cli_processes() -> Result<()> {
    let f = Fixture::new()?;
    let project = f.call(&["create", "first"])?["project"].clone();
    assert!(project.is_string());
    let expected = |model: &str| json!({"project":project,"model":model});
    assert_eq!(f.call(&["zcode-default"])?, expected(""));
    assert_eq!(
        f.call(&["zcode-default", "--model", MODEL_A])?,
        expected(MODEL_A)
    );
    assert_eq!(f.call(&["zcode-default"])?, expected(MODEL_A));
    assert_eq!(
        Library::open(&f.home)?.project().unwrap().zcode_default(),
        MODEL_A
    );
    assert_eq!(f.call(&["zcode-default", "--clear"])?, expected(""));
    assert_eq!(f.call(&["zcode-default"])?, expected(""));
    assert_eq!(
        Library::open(&f.home)?.project().unwrap().zcode_default(),
        ""
    );
    assert_eq!(f.call(&["zcode-default", "--clear"])?, expected(""));
    // Reading and clearing never need to consult the client catalog.
    assert_eq!(f.catalog_calls()?, 1);
    Ok(())
}

#[test]
fn default_projects_are_isolated_when_selecting_and_switching() -> Result<()> {
    let f = Fixture::new()?;
    let first = f.call(&["create", "first"])?["project"]
        .as_str()
        .unwrap()
        .to_owned();
    f.call(&["zcode-default", "--model", MODEL_A])?;
    let second = f.call(&["create", "second"])?["project"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(first, second);
    assert_eq!(
        f.call(&["zcode-default"])?,
        json!({"project":second,"model":""})
    );
    f.call(&["zcode-default", "--model", MODEL_B])?;
    assert_eq!(
        f.call(&["--project", &first, "zcode-default"])?,
        json!({"project":first,"model":MODEL_A})
    );
    // Explicit reads do not switch the active project.
    assert_eq!(
        f.call(&["zcode-default"])?,
        json!({"project":second,"model":MODEL_B})
    );
    for (project, model) in [(&first, MODEL_A), (&second, MODEL_B), (&first, MODEL_A)] {
        f.call(&["--project", project, "open"])?;
        assert_eq!(
            f.call(&["zcode-default"])?,
            json!({"project":project,"model":model})
        );
        let library = Library::open(&f.home)?;
        assert_eq!(library.project().unwrap().id, *project);
        assert_eq!(library.project().unwrap().zcode_default(), model);
    }
    f.call(&["--project", &second, "zcode-default", "--clear"])?;
    assert_eq!(f.call(&["zcode-default"])?["model"], MODEL_A);
    assert_eq!(
        f.call(&["--project", &second, "zcode-default"])?,
        json!({"project":second,"model":""})
    );
    Ok(())
}

#[test]
fn default_catalog_accepts_only_available_provider_model_and_level() -> Result<()> {
    let f = Fixture::new()?;
    f.call(&["create", "catalog"])?;
    assert_eq!(f.call(&["zcode-models"])?, support::catalog());
    for model in ["team/qa@low", MODEL_A, MODEL_B, "other/qa@low"] {
        assert_eq!(
            f.call(&["zcode-default", "--model", model])?["model"],
            model
        );
        assert_eq!(f.call(&["zcode-default"])?["model"], model);
    }
    assert_eq!(f.catalog_calls()?, 5);
    Ok(())
}

#[test]
fn default_rejects_unavailable_values_without_changing_saved_default() -> Result<()> {
    let f = Fixture::new()?;
    let project = f.call(&["create", "validation"])?["project"].clone();
    f.call(&["zcode-default", "--model", MODEL_A])?;
    let saved = fs::read(f.home.join("projects.json"))?;
    let unavailable = [
        "missing/qa@high",
        "team/missing@high",
        "team/qa@max",
        "other/qa@high",   // high exists only on the other provider's qa model.
        "team/other@high", // This model has no reasoning levels.
        "team/qa",         // This model requires a reasoning level.
        "@high",
        "",
    ];
    for model in unavailable {
        let output = f.invoke(&["zcode-default", "--model", model])?;
        assert!(!output.status.success(), "accepted unavailable {model:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("本机 ZCode 未提供此模型或档位"),
            "{model:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(f.home.join("projects.json"))?, saved);
        assert_eq!(
            f.call(&["zcode-default"])?,
            json!({"project":project,"model":MODEL_A})
        );
    }
    assert_eq!(f.catalog_calls()?, 1 + unavailable.len());
    Ok(())
}

// T2 tests the root runner's durable request object. It lives in
// evidence.json["request"]; `aep run` does not emit a separate request.json.
// Studio prefill, retry selection and GUI rendering remain T3 coverage.
#[test]
fn round_override_records_model_without_changing_project_default() -> Result<()> {
    let f = Fixture::repository()?;
    f.enable_execution_stub()?;
    let saved_project = fs::read(f.home.join("projects.json"))?;
    let evidence = f.run("round-override", Some(MODEL_B), None)?;
    assert_eq!(evidence["status"], "awaiting_human_review");
    assert_eq!(evidence["request"]["zcode_model"], MODEL_B);
    assert_eq!(evidence["implementation_source"], "zcode");
    assert_eq!(evidence["reviewer"], "zcode");
    assert_eq!(fs::read(f.home.join("projects.json"))?, saved_project);
    assert_eq!(f.call(&["zcode-default"])?["model"], MODEL_A);

    let calls = f.run_calls()?;
    let creates: Vec<_> = calls.iter().filter(|c| c["kind"] == "create").collect();
    assert_eq!(creates.len(), 2);
    for (create, mode) in creates.iter().zip(["edit", "plan"]) {
        assert_eq!(create["message"]["params"]["mode"], mode);
        assert_eq!(
            create["message"]["params"]["model"],
            json!({"providerId":"team","modelId":"other"})
        );
    }
    let sends: Vec<_> = calls.iter().filter(|c| c["kind"] == "send").collect();
    assert_eq!(sends.len(), 2);
    for send in sends {
        assert_eq!(
            send["message"]["params"]["modelSelection"],
            json!({"providerId":"team","modelId":"other"})
        );
    }
    Ok(())
}

#[test]
fn native_second_round_omits_model_and_preserves_first_round_evidence() -> Result<()> {
    let f = Fixture::repository()?;
    f.enable_execution_stub()?;
    let saved_project = fs::read(f.home.join("projects.json"))?;
    let first_id = "relay-models-r1-1";
    let second_id = "relay-models-r2-2";
    let first = f.run(first_id, Some(MODEL_B), None)?;
    assert_eq!(fs::read(f.home.join("projects.json"))?, saved_project);
    let first_bytes = fs::read(f.run_dir(first_id)?.join("evidence.json"))?;
    let second = f.run(second_id, None, Some(first_id))?;
    let second_bytes = fs::read(f.run_dir(second_id)?.join("evidence.json"))?;
    assert_eq!(first["status"], "awaiting_human_review");
    assert_eq!(second["status"], "awaiting_human_review");
    assert_eq!(first["request"]["zcode_model"], MODEL_B);
    assert!(first["request"].get("reviewer_model").is_none());
    assert!(second["request"].is_object());
    assert!(second["request"].get("zcode_model").is_none());
    assert!(second["request"].get("reviewer_model").is_none());
    assert_eq!(second["request"]["handoff_from_run"], first_id);
    assert_eq!(second["requested_candidate_sha"], first["candidate_sha"]);
    assert_eq!(
        fs::read(f.run_dir(first_id)?.join("evidence.json"))?,
        first_bytes
    );
    assert_eq!(fs::read(f.home.join("projects.json"))?, saved_project);
    assert_eq!(f.call(&["zcode-default"])?["model"], MODEL_A);
    assert_eq!(
        Library::open(&f.home)?.project().unwrap().zcode_default(),
        MODEL_A
    );

    let native: Vec<_> = f
        .run_calls()?
        .into_iter()
        .filter(|call| call["kind"] == "native")
        .collect();
    assert_eq!(
        native,
        vec![
            json!({"kind":"native","mode":"edit"}),
            json!({"kind":"native","mode":"plan"})
        ]
    );

    f.link_relay(
        "relay-models",
        &[(first_id, "First"), (second_id, "Second")],
    )?;
    // Separate CLI processes reopen both the project catalog and run evidence.
    // The timeline must use each historical request, despite default A on disk.
    for _ in 0..2 {
        let timeline = f.call(&["timeline"])?;
        assert_eq!(timeline["errors"], json!([]));
        let rounds = timeline["rounds"].as_array().unwrap();
        assert_eq!(rounds.len(), 2);
        for (round, id, number, expected) in [
            (
                &rounds[0],
                first_id,
                1,
                first["request"]["zcode_model"].clone(),
            ),
            (
                &rounds[1],
                second_id,
                2,
                second["request"]["zcode_model"].clone(),
            ),
        ] {
            assert_eq!(round["run"], id);
            assert_eq!(round["round"], number);
            assert_eq!(round["attempt"], 1);
            assert_eq!(round["status"], "awaiting_human_review");
            let agents = round["agents"].as_array().unwrap();
            assert_eq!(agents.len(), 2);
            for (agent, role) in agents.iter().zip(["实现", "审查"]) {
                assert_eq!(agent["role"], role);
                assert_eq!(agent["client"], "zcode");
                assert_eq!(agent["model"], expected);
            }
        }
    }
    // Reopening the timeline must not backfill the project default into either
    // historical request, including the round that used the client's default.
    for (id, original) in [(first_id, first_bytes), (second_id, second_bytes)] {
        assert_eq!(fs::read(f.run_dir(id)?.join("evidence.json"))?, original);
    }
    Ok(())
}

#[test]
fn timeline_reviewer_model_takes_precedence_with_zcode_fallback() -> Result<()> {
    let f = Fixture::repository()?;
    // Construct persisted evidence to cover precedence independently of model
    // execution, including an explicit choice of the reviewer's native default.
    let cases = [
        (
            "relay-priority-r1-1",
            json!({"zcode_model":MODEL_B}),
            json!(MODEL_B),
            json!(MODEL_B),
        ),
        (
            "relay-priority-r1-2",
            json!({"zcode_model":MODEL_B,"reviewer_model":MODEL_A}),
            json!(MODEL_B),
            json!(MODEL_A),
        ),
        (
            "relay-priority-r2-3",
            json!({"zcode_model":MODEL_B,"reviewer_model":""}),
            json!(MODEL_B),
            json!(null),
        ),
        (
            "relay-priority-r2-4",
            json!({"reviewer_model":MODEL_B}),
            json!(null),
            json!(MODEL_B),
        ),
        (
            "relay-priority-r2-5",
            json!({"zcode_model":MODEL_B,"reviewer_model":null}),
            json!(MODEL_B),
            json!(MODEL_B),
        ),
        (
            "relay-priority-r2-6",
            json!({"zcode_model":""}),
            json!(null),
            json!(null),
        ),
    ];
    for (id, request, _, _) in &cases {
        f.save_evidence(id, Some(request.clone()))?;
    }
    f.link_relay(
        "relay-priority",
        &[
            (cases[0].0, "First"),
            (cases[1].0, "First"),
            (cases[2].0, "Second"),
            (cases[3].0, "Second"),
            (cases[4].0, "Second"),
            (cases[5].0, "Second"),
        ],
    )?;
    let timeline = f.call(&["timeline"])?;
    assert_eq!(timeline["errors"], json!([]));
    let rounds = timeline["rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), cases.len());
    for (round, (id, request, implementation, review)) in rounds.iter().zip(cases) {
        assert_eq!(round["run"], id);
        assert_eq!(round["agents"][0]["role"], "实现");
        assert_eq!(round["agents"][0]["client"], "zcode");
        assert_eq!(round["agents"][0]["model"], implementation);
        assert_eq!(round["agents"][1]["role"], "审查");
        assert_eq!(round["agents"][1]["client"], "zcode");
        assert_eq!(round["agents"][1]["model"], review);
        assert_eq!(f.evidence(id)?["request"], request);
    }
    Ok(())
}

#[test]
fn legacy_relay_without_model_fields_reopens_without_displaying_project_default() -> Result<()> {
    let f = Fixture::repository()?;
    let first = "relay-legacy-models-r1-1";
    let second = "relay-legacy-models-r2-2";
    f.save_evidence(first, Some(json!({"task":"sample","reviewer":"zcode"})))?;
    // Older evidence can also predate the entire request object.
    f.save_evidence(second, None)?;
    f.link_relay(
        "relay-legacy-models",
        &[(first, "First"), (second, "Second")],
    )?;
    let manifest_path = f.data()?.join("relays/relay-legacy-models/relay.json");
    let manifest_bytes = fs::read(&manifest_path)?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    assert!(manifest.get("agents").is_none());
    let original = [
        fs::read(f.run_dir(first)?.join("evidence.json"))?,
        fs::read(f.run_dir(second)?.join("evidence.json"))?,
    ];
    assert!(f.evidence(first)?["request"].get("zcode_model").is_none());
    assert!(
        f.evidence(first)?["request"]
            .get("reviewer_model")
            .is_none()
    );
    assert!(f.evidence(second)?.get("request").is_none());
    for _ in 0..2 {
        let timeline = f.call(&["timeline"])?;
        assert_eq!(timeline["errors"], json!([]));
        let rounds = timeline["rounds"].as_array().unwrap();
        assert_eq!(rounds.len(), 2);
        for (round, id) in rounds.iter().zip([first, second]) {
            assert_eq!(round["run"], id);
            assert_eq!(round["status"], "awaiting_human_review");
            let agents = round["agents"].as_array().unwrap();
            assert_eq!(agents.len(), 2);
            assert!(
                agents
                    .iter()
                    .all(|a| a["client"] == "zcode" && a["model"].is_null())
            );
        }
    }
    for (id, bytes) in [first, second].into_iter().zip(original) {
        assert_eq!(fs::read(f.run_dir(id)?.join("evidence.json"))?, bytes);
    }
    assert_eq!(fs::read(&manifest_path)?, manifest_bytes);
    assert_eq!(f.call(&["zcode-default"])?["model"], MODEL_A);
    Ok(())
}
