PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS notes (
    id INTEGER PRIMARY KEY,
    scope TEXT NOT NULL, task TEXT NOT NULL, spec_sha TEXT NOT NULL,
    candidate_sha TEXT NOT NULL, origin_sha TEXT NOT NULL,
    kind TEXT NOT NULL, author TEXT NOT NULL, run_id TEXT,
    body TEXT NOT NULL, created_at TEXT NOT NULL,
    replaces INTEGER UNIQUE REFERENCES notes(id),
    UNIQUE(run_id, author, kind)
);
CREATE INDEX IF NOT EXISTS notes_context ON notes(task, spec_sha, candidate_sha, id);
