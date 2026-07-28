use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Option<i64>,
    pub ledger: u32,
    pub ledger_closed_at: String,
    pub tx_hash: String,
    pub contract_id: String,
    pub category: String,
    pub action: String,
    pub value_json: String,
    pub raw_topics: Option<String>,
    pub raw_value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerHash {
    pub sequence: u32,
    pub hash: String,
}

const SCHEMA_VERSION: i64 = 1;

pub fn open_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .context("Failed to open SQLite database")?;
    // busy_timeout tells SQLite to retry for up to 5 s on SQLITE_BUSY before
    // returning an error.  Without it, any brief writer/reader contention during
    // a WAL checkpoint yields an immediate error that callers have no way to
    // recover from.  Must be set before journal_mode so it also applies to the
    // WAL-mode transition itself.
    conn.execute_batch(
        "PRAGMA busy_timeout   = 5000;\
         PRAGMA journal_mode   = WAL;\
         PRAGMA synchronous    = NORMAL;",
    )
    .context("Failed to set PRAGMAs")?;
    Ok(conn)
}

pub fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);

         CREATE TABLE IF NOT EXISTS cursor (
             key   TEXT PRIMARY KEY NOT NULL,
             value TEXT NOT NULL
         );

         CREATE TABLE IF NOT EXISTS ledger_hashes (
             sequence INTEGER PRIMARY KEY NOT NULL,
             hash     TEXT NOT NULL,
             seen_at  TEXT NOT NULL DEFAULT (datetime('now'))
         );

         CREATE TABLE IF NOT EXISTS events (
             id               INTEGER PRIMARY KEY AUTOINCREMENT,
             ledger           INTEGER NOT NULL,
             ledger_closed_at TEXT NOT NULL,
             tx_hash          TEXT NOT NULL,
             contract_id      TEXT NOT NULL,
             category         TEXT NOT NULL,
             action           TEXT NOT NULL,
             value_json       TEXT NOT NULL,
             raw_topics       TEXT,
             raw_value        TEXT,
             ingested_at      TEXT NOT NULL DEFAULT (datetime('now'))
         );

         CREATE INDEX IF NOT EXISTS idx_events_ledger         ON events(ledger);
         CREATE INDEX IF NOT EXISTS idx_events_cat_action     ON events(category, action);
         CREATE INDEX IF NOT EXISTS idx_events_tx_hash        ON events(tx_hash);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_events_dedup   ON events(ledger, tx_hash, category, action);

         CREATE TABLE IF NOT EXISTS reorg_audit (
             id                INTEGER PRIMARY KEY AUTOINCREMENT,
             ledger            INTEGER NOT NULL,
             expected_hash     TEXT NOT NULL,
             actual_hash       TEXT NOT NULL,
             rolled_back_at    TEXT NOT NULL DEFAULT (datetime('now'))
         );

         CREATE VIEW IF NOT EXISTS vouch_events AS
         SELECT id, ledger, ledger_closed_at, tx_hash,
                json_extract(value_json, '$.voucher')       AS voucher,
                json_extract(value_json, '$.borrower')      AS borrower,
                json_extract(value_json, '$.stake_stroops') AS stake_stroops,
                json_extract(value_json, '$.token')         AS token,
                action,
                ingested_at
         FROM events WHERE category = 'vouch';

         CREATE VIEW IF NOT EXISTS loan_events AS
         SELECT id, ledger, ledger_closed_at, tx_hash,
                json_extract(value_json, '$.borrower')         AS borrower,
                json_extract(value_json, '$.amount_stroops')   AS amount_stroops,
                json_extract(value_json, '$.threshold_stroops') AS threshold_stroops,
                json_extract(value_json, '$.loan_purpose')     AS loan_purpose,
                json_extract(value_json, '$.token')            AS token,
                action,
                ingested_at
         FROM events WHERE category = 'loan';",
    )
    .context("Failed to run migrations")?;

    let version: i64 = conn
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0))
        .unwrap_or(0);

    if version < SCHEMA_VERSION {
        conn.execute("INSERT OR REPLACE INTO schema_version (version) VALUES (?1)", params![SCHEMA_VERSION])
            .context("Failed to update schema version")?;
    }

    Ok(())
}

pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = open_db(path)?;
        run_migrations(&conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub async fn get_cursor(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT value FROM cursor WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get::<_, String>(0)?)),
            None => Ok(None),
        }
    }

    pub async fn set_cursor(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO cursor (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub async fn get_last_ledger(&self) -> Result<Option<u32>> {
        let val = self.get_cursor("last_ledger").await?;
        match val {
            Some(s) => Ok(Some(s.parse::<u32>()?)),
            None => Ok(None),
        }
    }

    pub async fn set_last_ledger(&self, ledger: u32) -> Result<()> {
        self.set_cursor("last_ledger", &ledger.to_string()).await
    }

    pub async fn store_ledger_hash(&self, sequence: u32, hash: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO ledger_hashes (sequence, hash) VALUES (?1, ?2)",
            params![sequence, hash],
        )?;
        Ok(())
    }

    pub async fn get_ledger_hash(&self, sequence: u32) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT hash FROM ledger_hashes WHERE sequence = ?1")?;
        let mut rows = stmt.query(params![sequence])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get::<_, String>(0)?)),
            None => Ok(None),
        }
    }

    pub async fn insert_event(&self, event: &Event) -> Result<bool> {
        let conn = self.conn.lock().await;
        let result = conn.execute(
            "INSERT OR IGNORE INTO events
             (ledger, ledger_closed_at, tx_hash, contract_id, category, action, value_json, raw_topics, raw_value)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                event.ledger,
                event.ledger_closed_at,
                event.tx_hash,
                event.contract_id,
                event.category,
                event.action,
                event.value_json,
                event.raw_topics,
                event.raw_value,
            ],
        )?;
        Ok(result > 0)
    }

    pub async fn rollback_from_ledger(&self, from_ledger: u32, expected_hash: &str, actual_hash: &str) -> Result<u64> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO reorg_audit (ledger, expected_hash, actual_hash) VALUES (?1, ?2, ?3)",
            params![from_ledger, expected_hash, actual_hash],
        )?;
        let deleted = conn.execute(
            "DELETE FROM events WHERE ledger >= ?1",
            params![from_ledger],
        )? as u64;
        conn.execute("DELETE FROM cursor WHERE key = 'last_ledger'", [])?;
        conn.execute(
            "INSERT OR REPLACE INTO cursor (key, value) VALUES ('last_ledger', ?1)",
            params![(from_ledger.saturating_sub(1))],
        )?;
        conn.execute("DELETE FROM ledger_hashes WHERE sequence >= ?1", params![from_ledger])?;
        Ok(deleted)
    }

    pub async fn get_events_since(&self, ledger: u32) -> Result<Vec<Event>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, ledger, ledger_closed_at, tx_hash, contract_id, category, action, value_json, raw_topics, raw_value
             FROM events WHERE ledger >= ?1 ORDER BY ledger ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![ledger], |row| {
            Ok(Event {
                id: Some(row.get(0)?),
                ledger: row.get(1)?,
                ledger_closed_at: row.get(2)?,
                tx_hash: row.get(3)?,
                contract_id: row.get(4)?,
                category: row.get(5)?,
                action: row.get(6)?,
                value_json: row.get(7)?,
                raw_topics: row.get(8)?,
                raw_value: row.get(9)?,
            })
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    pub async fn get_all_events(&self) -> Result<Vec<Event>> {
        self.get_events_since(0).await
    }

    pub async fn get_latest_sequence_with_hash(&self) -> Result<Option<(u32, String)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT sequence, hash FROM ledger_hashes ORDER BY sequence DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    }

    pub async fn count_events_by_category_action(&self) -> Result<Vec<(String, String, i64)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT category, action, COUNT(*) as cnt FROM events GROUP BY category, action",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    pub async fn sum_loan_volume(&self) -> Result<f64> {
        let conn = self.conn.lock().await;
        let val: Option<f64> = conn
            .query_row(
                "SELECT SUM(json_extract(value_json, '$.amount_stroops'))
                 FROM events WHERE category = 'loan' AND action = 'request'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(val.unwrap_or(0.0))
    }

    pub async fn sum_slash_amount(&self) -> Result<f64> {
        let conn = self.conn.lock().await;
        let val: Option<f64> = conn
            .query_row(
                "SELECT SUM(json_extract(value_json, '$.total_slashed_stroops'))
                 FROM events WHERE category = 'loan' AND action = 'slash'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(val.unwrap_or(0.0))
    }

    /// Issue #1146: every distinct borrower address that has ever appeared in
    /// a `loan` or `vouch` event — the ground-truth address set `backup.sh`
    /// derives its snapshot targets from, instead of a manually-maintained
    /// `BORROWER_ADDRESSES` env var. Captures any borrower the indexer has
    /// observed, whether or not an operator ever added them to a config file.
    pub async fn distinct_borrower_addresses(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT borrower FROM (
                 SELECT json_extract(value_json, '$.borrower') AS borrower FROM events WHERE category = 'loan'
                 UNION
                 SELECT json_extract(value_json, '$.borrower') AS borrower FROM events WHERE category = 'vouch'
             ) WHERE borrower IS NOT NULL ORDER BY borrower ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut addresses = Vec::new();
        for row in rows {
            addresses.push(row?);
        }
        Ok(addresses)
    }

    pub async fn count_events_by_action(&self, category: &str, action: &str) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM events WHERE category = ?1 AND action = ?2",
            params![category, action],
            |r| r.get(0),
        )
        .map_err(Into::into)
    }
}

trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
