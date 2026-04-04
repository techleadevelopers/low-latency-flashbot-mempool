use crate::dashboard::DashboardEvent;
use chrono::Utc;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
}

impl Storage {
    pub fn new(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                at TEXT NOT NULL,
                level TEXT NOT NULL,
                message TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sweeps (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                at TEXT NOT NULL,
                wallet TEXT NOT NULL,
                rpc TEXT,
                status TEXT NOT NULL,
                detail TEXT
            );

            CREATE TABLE IF NOT EXISTS telemetry (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                at TEXT NOT NULL,
                stage TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                wallet TEXT,
                note TEXT
            );

            CREATE TABLE IF NOT EXISTS wallet_residual_stats (
                wallet TEXT PRIMARY KEY,
                last_seen_at TEXT NOT NULL,
                asset_class TEXT NOT NULL,
                detections INTEGER NOT NULL DEFAULT 0,
                successful_sweeps INTEGER NOT NULL DEFAULT 0,
                small_positive_detections INTEGER NOT NULL DEFAULT 0,
                total_residual_wei TEXT NOT NULL DEFAULT '0',
                detected_profit_wei TEXT NOT NULL DEFAULT '0',
                realized_profit_wei TEXT NOT NULL DEFAULT '0'
            );
            "#,
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn log_event(&self, level: &str, message: &str) {
        let now = Utc::now().to_rfc3339();
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO events (at, level, message) VALUES (?1, ?2, ?3)",
                params![now, level, message],
            );
        }
    }

    pub fn log_sweep(&self, wallet: &str, rpc: &str, status: &str, detail: Option<&str>) {
        let now = Utc::now().to_rfc3339();
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO sweeps (at, wallet, rpc, status, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![now, wallet, rpc, status, detail],
            );
        }
    }

    pub fn log_telemetry(
        &self,
        stage: &str,
        duration_ms: u128,
        wallet: Option<&str>,
        note: Option<&str>,
    ) {
        let now = Utc::now().to_rfc3339();
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO telemetry (at, stage, duration_ms, wallet, note) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![now, stage, duration_ms as i64, wallet, note],
            );
        }
    }

    pub fn recent_events(
        &self,
        limit: usize,
    ) -> Result<Vec<DashboardEvent>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().map_err(|_| "storage lock poisoned")?;
        let mut stmt =
            conn.prepare("SELECT at, level, message FROM events ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(DashboardEvent {
                at: row.get(0)?,
                level: row.get(1)?,
                message: row.get(2)?,
            })
        })?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    pub fn sweep_counts(&self) -> Result<(u64, u64, u64), Box<dyn std::error::Error>> {
        let conn = self.conn.lock().map_err(|_| "storage lock poisoned")?;
        let attempted: u64 = conn.query_row("SELECT COUNT(*) FROM sweeps", [], |row| {
            row.get::<_, u64>(0)
        })?;
        let succeeded: u64 = conn.query_row(
            "SELECT COUNT(*) FROM sweeps WHERE status = 'success'",
            [],
            |row| row.get::<_, u64>(0),
        )?;
        let failed: u64 = conn.query_row(
            "SELECT COUNT(*) FROM sweeps WHERE status = 'failed'",
            [],
            |row| row.get::<_, u64>(0),
        )?;
        Ok((attempted, succeeded, failed))
    }

    pub fn telemetry_summary(
        &self,
    ) -> Result<HashMap<String, (u64, u128, u128, u128)>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().map_err(|_| "storage lock poisoned")?;
        let mut summary = HashMap::new();

        let mut stmt = conn.prepare(
            "SELECT stage, COUNT(*), AVG(duration_ms), MAX(duration_ms)
             FROM telemetry
             GROUP BY stage",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, f64>(2)? as u128,
                row.get::<_, i64>(3)? as u128,
            ))
        })?;

        for row in rows {
            let (stage, samples, avg_ms, max_ms) = row?;
            summary.insert(stage, (samples, 0, avg_ms, max_ms));
        }

        let mut stmt = conn.prepare(
            "SELECT t.stage, t.duration_ms
             FROM telemetry t
             INNER JOIN (
                SELECT stage, MAX(id) AS last_id
                FROM telemetry
                GROUP BY stage
             ) latest ON latest.stage = t.stage AND latest.last_id = t.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u128))
        })?;

        for row in rows {
            let (stage, last_ms) = row?;
            if let Some(entry) = summary.get_mut(&stage) {
                entry.1 = last_ms;
            }
        }

        Ok(summary)
    }

    pub fn record_residual_detection(
        &self,
        wallet: &str,
        asset_class: &str,
        total_residual_wei: &str,
        detected_profit_wei: &str,
        is_small_positive: bool,
    ) {
        let now = Utc::now().to_rfc3339();
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                r#"
                INSERT INTO wallet_residual_stats (
                    wallet, last_seen_at, asset_class, detections, successful_sweeps,
                    small_positive_detections, total_residual_wei, detected_profit_wei, realized_profit_wei
                )
                VALUES (?1, ?2, ?3, 1, 0, ?4, ?5, ?6, '0')
                ON CONFLICT(wallet) DO UPDATE SET
                    last_seen_at=excluded.last_seen_at,
                    asset_class=excluded.asset_class,
                    detections=detections + 1,
                    small_positive_detections=small_positive_detections + excluded.small_positive_detections,
                    total_residual_wei=CAST(total_residual_wei AS INTEGER) + CAST(excluded.total_residual_wei AS INTEGER),
                    detected_profit_wei=CAST(detected_profit_wei AS INTEGER) + CAST(excluded.detected_profit_wei AS INTEGER)
                "#,
                params![
                    wallet,
                    now,
                    asset_class,
                    if is_small_positive { 1 } else { 0 },
                    total_residual_wei,
                    detected_profit_wei
                ],
            );
        }
    }

    pub fn record_residual_success(&self, wallet: &str, realized_profit_wei: &str) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                r#"
                UPDATE wallet_residual_stats
                SET
                    successful_sweeps=successful_sweeps + 1,
                    realized_profit_wei=CAST(realized_profit_wei AS INTEGER) + CAST(?2 AS INTEGER)
                WHERE wallet=?1
                "#,
                params![wallet, realized_profit_wei],
            );
        }
    }

    pub fn top_wallet_residuals(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, String, u64, u64, String, String, String)>, Box<dyn std::error::Error>>
    {
        let conn = self.conn.lock().map_err(|_| "storage lock poisoned")?;
        let mut stmt = conn.prepare(
            r#"
            SELECT wallet, asset_class, detections, successful_sweeps,
                   detected_profit_wei, realized_profit_wei, last_seen_at
            FROM wallet_residual_stats
            ORDER BY CAST(detected_profit_wei AS INTEGER) DESC, detections DESC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, u64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;

        let mut stats = Vec::new();
        for row in rows {
            stats.push(row?);
        }
        Ok(stats)
    }
}
