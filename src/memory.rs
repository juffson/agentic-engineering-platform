use crate::workspace::{commit, git, ident};
use anyhow::{Context, Result, anyhow, ensure};
use chrono::Utc;
use clap::ValueEnum;
use rusqlite::{Connection, Row, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::Duration,
};

const SCHEMA: &str = include_str!("memory.sql");
#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum Kind {
    Decision,
    Handoff,
    OpenQuestion,
    Review,
}
impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Handoff => "handoff",
            Self::OpenQuestion => "open_question",
            Self::Review => "review",
        }
    }
}

pub struct Key {
    pub task: String,
    pub spec: String,
    pub candidate: String,
}
impl Key {
    pub fn new(task: &str, spec: &str, candidate: &str) -> Self {
        Self {
            task: task.into(),
            spec: spec.into(),
            candidate: candidate.into(),
        }
    }
    pub fn resolve(
        repo: &Path,
        task: Option<&str>,
        spec: Option<&str>,
        candidate: Option<&str>,
    ) -> Result<Self> {
        let (task, spec) = match (task, spec, candidate) {
            (None, None, None) => return Ok(Self::new("", "", "")),
            (Some(task), Some(spec), _) => (task, spec),
            _ => anyhow::bail!("task and spec must be supplied together"),
        };
        ident(task, 63)?;
        let spec = commit(repo, spec).context("resolve spec commit")?;
        let candidate = candidate
            .map(|rev| commit(repo, rev).context("resolve candidate commit"))
            .transpose()?;
        if let Some(candidate) = &candidate {
            git(repo, &["merge-base", "--is-ancestor", &spec, candidate])
                .context("candidate must descend from spec")?;
        }
        Ok(Self::new(task, &spec, candidate.as_deref().unwrap_or("")))
    }
    fn scope(&self) -> &str {
        if !self.candidate.is_empty() {
            "candidate"
        } else if !self.task.is_empty() {
            "task"
        } else {
            "project"
        }
    }
}
#[derive(Debug)]
pub struct RevisionConflict;

impl std::fmt::Display for RevisionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("replaced memory must be current and in the same scope")
    }
}

impl std::error::Error for RevisionConflict {}

pub struct Memory(Connection);
pub const TEXT_LIMIT: usize = 1000;

fn note(row: &Row<'_>) -> rusqlite::Result<Value> {
    let mut v = json!({"id":row.get::<_,i64>("id")?,"run_id":row.get::<_,Option<String>>("run_id")?,"replaces":row.get::<_,Option<i64>>("replaces")?});
    for col in [
        "scope",
        "task",
        "spec_sha",
        "candidate_sha",
        "origin_sha",
        "kind",
        "author",
        "body",
        "created_at",
    ] {
        v[col] = json!(row.get::<_, String>(col)?);
    }
    Ok(v)
}
fn text(s: &str, optional: bool) -> Result<&str> {
    let s = s.trim();
    ensure!(
        s.chars().count() <= TEXT_LIMIT && (optional || !s.is_empty()),
        "invalid memory text (limit {TEXT_LIMIT} characters)"
    );
    Ok(s)
}
impl Memory {
    /// Browse the workspace without initializing a database or replacing historical notes.
    pub fn browse(dir: &Path) -> Result<Vec<Value>> {
        let path = dir.join("memory.sqlite3");
        if !path.try_exists()? {
            return Ok(Vec::new());
        }
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut query = db.prepare(
            "SELECT n.*, (SELECT id FROM notes s WHERE s.replaces=n.id) AS successor
             FROM notes n ORDER BY n.id DESC",
        )?;
        Ok(query
            .query_map([], |row| {
                let mut value = note(row)?;
                value["successor"] = json!(row.get::<_, Option<i64>>("successor")?);
                Ok(value)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn read_only(dir: &Path) -> Result<Option<Self>> {
        let file = dir.join("memory.sqlite3");
        if !file.try_exists()? {
            return Ok(None);
        }
        let db = Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(Duration::from_secs(5))?;
        Ok(Some(Self(db)))
    }
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join("memory.sqlite3");
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(10))?;
        db.execute_batch(SCHEMA)?;
        Ok(Self(db))
    }
    pub fn get(&self, id: i64) -> Result<Value> {
        Ok(self
            .0
            .query_row("SELECT * FROM notes WHERE id=?", [id], note)?)
    }
    pub fn add(&mut self, k: &Key, kind: &str, body: &str, replaces: Option<i64>) -> Result<i64> {
        self.add_manual(k, kind, body, replaces, None)
    }
    pub fn add_manual(
        &mut self,
        k: &Key,
        kind: &str,
        body: &str,
        replaces: Option<i64>,
        run: Option<&str>,
    ) -> Result<i64> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(old) = replaces {
            let active: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM notes WHERE id=?1 AND scope=?2 AND task=?3
                    AND spec_sha=?4 AND candidate_sha=?5
                    AND NOT EXISTS(SELECT 1 FROM notes WHERE replaces=?1)
                 )",
                params![old, k.scope(), k.task, k.spec, k.candidate],
                |row| row.get(0),
            )?;
            ensure!(active, RevisionConflict);
        }
        tx.execute(
            "INSERT INTO notes(scope,task,spec_sha,candidate_sha,origin_sha,kind,author,body,created_at,replaces,run_id)
             VALUES(?1,?2,?3,?4,?4,?5,'manual',?6,?7,?8,?9)",
            params![k.scope(), k.task, k.spec, k.candidate, kind, text(body,false)?, Utc::now().to_rfc3339(), replaces, run],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }
    /// Use the scope recorded on the original note, including when its Git refs have moved.
    pub fn revise(&mut self, id: i64, kind: Kind, body: &str) -> Result<i64> {
        let key = self
            .0
            .query_row(
                "SELECT task,spec_sha,candidate_sha FROM notes WHERE id=?",
                [id],
                |row| {
                    Ok(Key {
                        task: row.get(0)?,
                        spec: row.get(1)?,
                        candidate: row.get(2)?,
                    })
                },
            )
            .context("read original memory")?;
        self.add(&key, kind.as_str(), body, Some(id))
    }
    /// Take a note out of every context agents read, keeping it in history: a tombstone
    /// successor in a scope no context query includes.
    pub fn retire(&mut self, id: i64, why: &str) -> Result<i64> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM notes WHERE replaces=?1))",
            [id],
            |row| row.get(0),
        )?;
        ensure!(current, RevisionConflict);
        tx.execute(
            "INSERT INTO notes(scope,task,spec_sha,candidate_sha,origin_sha,kind,author,body,created_at,replaces,run_id)
             SELECT 'retired',task,spec_sha,candidate_sha,origin_sha,kind,'retired',?2,?3,id,NULL FROM notes WHERE id=?1",
            params![id, text(why, false)?, Utc::now().to_rfc3339()],
        )?;
        let new = tx.last_insert_rowid();
        tx.commit()?;
        Ok(new)
    }
    pub fn context(&self, k: &Key, run: Option<&str>) -> Result<Vec<Value>> {
        self.context_with_required(k, run, None)
    }

    pub fn context_with_required(
        &self,
        k: &Key,
        run: Option<&str>,
        required: Option<i64>,
    ) -> Result<Vec<Value>> {
        self.context_with_handoff(k, run, required, None)
    }

    pub fn context_with_handoff(
        &self,
        k: &Key,
        run: Option<&str>,
        required: Option<i64>,
        previous: Option<&str>,
    ) -> Result<Vec<Value>> {
        let mut stmt = self.0.prepare(
            "SELECT n.* FROM notes n
             WHERE (scope='project' OR (task=?1 AND spec_sha=?2 AND
                   (scope='task' OR (scope='candidate' AND (candidate_sha=?3 OR (run_id=?6 AND kind IN ('review','handoff')))))))
               AND NOT EXISTS(SELECT 1 FROM notes successor WHERE successor.replaces=n.id)
             ORDER BY CASE WHEN id=?5 THEN -3 WHEN run_id=?6 AND kind='review' THEN -2
                           WHEN run_id=?4 THEN -1 WHEN run_id=?6 AND kind='handoff' THEN 0 WHEN scope='candidate' THEN 1
                           WHEN scope='task' THEN 2 ELSE 3 END, id DESC LIMIT 48",
        )?;
        let mut result = Vec::new();
        let mut bytes = 2;
        for row in stmt.query_map(
            params![k.task, k.spec, k.candidate, run, required, previous],
            note,
        )? {
            let v = row?;
            let added = serde_json::to_vec(&v)?.len() + usize::from(!result.is_empty());
            if bytes + added > 8192
                && previous.is_some()
                && v["run_id"].as_str() == previous
                && v["kind"] == "review"
            {
                anyhow::bail!(
                    "the previous review and required decision exceed the shared memory budget"
                );
            }
            if bytes + added <= 8192 {
                bytes += added;
                result.push(v);
                if result.len() == 12 {
                    break;
                }
            }
        }
        if let Some(id) = required {
            ensure!(
                result.first().is_some_and(|note| note["id"] == id),
                "required memory #{id} is no longer current or does not apply to this candidate"
            );
        }
        Ok(result)
    }
    /// Current project decisions plus the incoming work-specific handoff.
    pub fn workflow_context(&self, previous: Option<&str>) -> Result<Vec<Value>> {
        let mut query=self.0.prepare("SELECT n.* FROM notes n WHERE scope='project' AND (author='manual' OR run_id=?1)
            AND NOT EXISTS(SELECT 1 FROM notes s WHERE s.replaces=n.id)
            ORDER BY CASE WHEN run_id=?1 THEN 0 WHEN kind='decision' THEN 1 ELSE 2 END,id DESC LIMIT 32")?;
        Ok(query
            .query_map([previous], note)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn workflow_delta(&self, previous: Option<&str>, since: i64) -> Result<Vec<Value>> {
        let mut stmt=self.0.prepare("SELECT n.* FROM notes n WHERE scope='project' AND id>?2 AND (author='manual' OR run_id=?1)
            AND NOT EXISTS(SELECT 1 FROM notes s WHERE s.replaces=n.id) ORDER BY id LIMIT 33")?;
        Ok(stmt
            .query_map(params![previous, since], note)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn project_handoff(&mut self, run: &str, author: &str, body: &str) -> Result<i64> {
        self.0.query_row("INSERT INTO notes(scope,task,spec_sha,candidate_sha,origin_sha,kind,author,run_id,body,created_at)
            VALUES('project','','','','','handoff',?1,?2,?3,?4)
            ON CONFLICT(run_id,author,kind) DO UPDATE SET body=excluded.body WHERE notes.body=excluded.body RETURNING id",
            params![author,run,text(body,false)?,Utc::now().to_rfc3339()],|r|r.get(0))
            .context("publish project handoff with a unique attempt ID")
    }
    pub fn publish(&mut self, role: &str, run: &str, k: &Key, payload: &Value) -> Result<Vec<i64>> {
        let field = |name: &str| {
            payload[name]
                .as_str()
                .ok_or_else(|| anyhow!("missing string field {name}"))
        };
        let summary = text(field("note")?, false)?;
        let question = text(field("question")?, true)?;
        let kind = if role == "implementer" {
            "handoff"
        } else {
            "review"
        };
        let mut entries = vec![("candidate", kind, k.candidate.as_str(), summary)];
        if !question.is_empty() {
            entries.push(("task", "open_question", "", question));
        }
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM notes WHERE run_id=? AND author=?",
            params![run, role],
            |r| r.get(0),
        )?;
        ensure!(
            existing == 0 || existing == entries.len() as i64,
            "conflicting publication for this run"
        );
        let mut ids = Vec::new();
        for (scope, kind, pin, body) in entries {
            tx.execute(
                "INSERT INTO notes(scope,task,spec_sha,candidate_sha,origin_sha,kind,author,run_id,body,created_at)
                 VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(run_id,author,kind) DO NOTHING",
                params![scope, k.task, k.spec, pin, k.candidate, kind, role, run, body, Utc::now().to_rfc3339()],
            )?;
            let row = tx.query_row(
                "SELECT * FROM notes WHERE run_id=? AND author=? AND kind=?",
                params![run, role, kind],
                note,
            )?;
            ensure!(
                row["body"] == body
                    && row["origin_sha"] == k.candidate
                    && row["task"] == k.task
                    && row["spec_sha"] == k.spec,
                "conflicting publication for this run"
            );
            ids.push(row["id"].as_i64().unwrap());
        }
        tx.commit()?;
        Ok(ids)
    }
}
