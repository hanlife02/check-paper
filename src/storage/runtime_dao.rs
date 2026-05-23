use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::{RuntimeHeartbeat, Storage};

impl Storage {
    pub fn save_runtime_heartbeat(&self, name: &str, status: &str) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO runtime_heartbeats (name, status, updated_at)
            VALUES (?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(name) DO UPDATE SET
                status = excluded.status,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![name, status],
        )?;
        Ok(())
    }

    pub fn runtime_heartbeat(&self, name: &str) -> Result<Option<RuntimeHeartbeat>> {
        self.conn
            .query_row(
                r#"
                SELECT name, status, updated_at,
                       CAST(strftime('%s', 'now') - strftime('%s', updated_at) AS INTEGER)
                FROM runtime_heartbeats
                WHERE name = ?
                "#,
                params![name],
                |row| {
                    Ok(RuntimeHeartbeat {
                        name: row.get(0)?,
                        status: row.get(1)?,
                        updated_at: row.get(2)?,
                        age_seconds: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::storage::Storage;

    #[test]
    fn saves_runtime_heartbeat_with_age() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();

        storage
            .save_runtime_heartbeat("telegram_polling", "polling")
            .unwrap();
        let heartbeat = storage
            .runtime_heartbeat("telegram_polling")
            .unwrap()
            .unwrap();

        assert_eq!(heartbeat.name, "telegram_polling");
        assert_eq!(heartbeat.status, "polling");
        assert!(heartbeat.age_seconds.unwrap_or_default() >= 0);
    }
}
