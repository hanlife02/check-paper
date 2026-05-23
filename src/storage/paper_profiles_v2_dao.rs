use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use super::{NewPaperProfileV2, PaperProfileV2Record, Storage};

impl Storage {
    pub fn save_paper_profile_v2(&self, profile: NewPaperProfileV2<'_>) -> Result<bool> {
        let changed = self.paper_profile_v2_changed(profile)?;
        self.conn.execute(
            r#"
            INSERT INTO paper_profiles_v2 (
                paper_key, profile_json, profile_schema_version, builder_version,
                model_id, source_fact_hash, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT(paper_key) DO UPDATE SET
                profile_json = excluded.profile_json,
                profile_schema_version = excluded.profile_schema_version,
                builder_version = excluded.builder_version,
                model_id = excluded.model_id,
                source_fact_hash = excluded.source_fact_hash,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![
                profile.paper_key,
                serde_json::to_string(profile.profile_json)?,
                profile.profile_schema_version,
                profile.builder_version,
                profile.model_id,
                profile.source_fact_hash,
            ],
        )?;
        Ok(changed)
    }

    pub fn paper_profile_v2_is_current(
        &self,
        paper_key: &str,
        profile_schema_version: i64,
        builder_version: &str,
        model_id: &str,
        source_fact_hash: &str,
    ) -> Result<bool> {
        let current: Option<i64> = self
            .conn
            .query_row(
                r#"
                SELECT 1
                FROM paper_profiles_v2
                WHERE paper_key = ?
                  AND profile_schema_version = ?
                  AND builder_version = ?
                  AND model_id = ?
                  AND source_fact_hash = ?
                "#,
                params![
                    paper_key,
                    profile_schema_version,
                    builder_version,
                    model_id,
                    source_fact_hash
                ],
                |row| row.get(0),
            )
            .optional()?;
        Ok(current.is_some())
    }

    pub fn paper_profile_v2(&self, paper_key: &str) -> Result<Option<PaperProfileV2Record>> {
        self.conn
            .query_row(
                r#"
                SELECT paper_key, profile_json, profile_schema_version, builder_version,
                       model_id, source_fact_hash, updated_at
                FROM paper_profiles_v2
                WHERE paper_key = ?
                "#,
                params![paper_key],
                paper_profile_v2_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn paper_profiles_v2_for_author(
        &self,
        author: &str,
        limit: Option<usize>,
    ) -> Result<Vec<PaperProfileV2Record>> {
        let mut sql = r#"
            SELECT v.paper_key, v.profile_json, v.profile_schema_version,
                   v.builder_version, v.model_id, v.source_fact_hash, v.updated_at
            FROM paper_profiles_v2 v
            JOIN papers p ON p.paper_key = v.paper_key
            WHERE p.author = ?
            ORDER BY p.year DESC, p.title ASC
        "#
        .to_string();
        if limit.is_some() {
            sql.push_str(" LIMIT ?");
        }
        let mut records = Vec::new();
        if let Some(limit) = limit {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![author, limit as i64], paper_profile_v2_from_row)?;
            for row in rows {
                records.push(row?);
            }
        } else {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![author], paper_profile_v2_from_row)?;
            for row in rows {
                records.push(row?);
            }
        }
        Ok(records)
    }

    fn paper_profile_v2_changed(&self, profile: NewPaperProfileV2<'_>) -> Result<bool> {
        let existing: Option<(String, i64, String, String, String)> = self
            .conn
            .query_row(
                r#"
                SELECT profile_json, profile_schema_version, builder_version, model_id,
                       source_fact_hash
                FROM paper_profiles_v2
                WHERE paper_key = ?
                "#,
                params![profile.paper_key],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((profile_json, schema_version, builder_version, model_id, source_fact_hash)) =
            existing
        else {
            return Ok(true);
        };
        Ok(profile_json != serde_json::to_string(profile.profile_json)?
            || schema_version != profile.profile_schema_version
            || builder_version != profile.builder_version
            || model_id != profile.model_id
            || source_fact_hash != profile.source_fact_hash)
    }
}

fn paper_profile_v2_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PaperProfileV2Record> {
    let profile_json: String = row.get(1)?;
    Ok(PaperProfileV2Record {
        paper_key: row.get(0)?,
        profile_json: serde_json::from_str::<Value>(&profile_json)
            .unwrap_or_else(|_| json!({ "raw_profile": profile_json })),
        profile_schema_version: row.get(2)?,
        builder_version: row.get(3)?,
        model_id: row.get(4)?,
        source_fact_hash: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn saves_and_checks_current_paper_profile_v2() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .conn
            .execute(
                r#"
                INSERT INTO papers (
                    paper_key, author, paper_id, title, doi, year, source_hash,
                    article_path, metadata_json, fetch_result_json
                )
                VALUES ('Alice/paper-a', 'Alice', 'paper-a', 'A Paper', '', '2024',
                        'source', 'article.md', '{}', '{}')
                "#,
                [],
            )
            .unwrap();
        let profile_json = json!({"paper_key": "Alice/paper-a"});
        let changed = storage
            .save_paper_profile_v2(NewPaperProfileV2 {
                paper_key: "Alice/paper-a",
                profile_json: &profile_json,
                profile_schema_version: 2,
                builder_version: "paper-profile-v2-s3",
                model_id: "deterministic",
                source_fact_hash: "facts",
            })
            .unwrap();

        assert!(changed);
        assert!(
            storage
                .paper_profile_v2_is_current(
                    "Alice/paper-a",
                    2,
                    "paper-profile-v2-s3",
                    "deterministic",
                    "facts"
                )
                .unwrap()
        );
        let unchanged = storage
            .save_paper_profile_v2(NewPaperProfileV2 {
                paper_key: "Alice/paper-a",
                profile_json: &profile_json,
                profile_schema_version: 2,
                builder_version: "paper-profile-v2-s3",
                model_id: "deterministic",
                source_fact_hash: "facts",
            })
            .unwrap();
        assert!(!unchanged);
        assert_eq!(
            storage
                .paper_profile_v2("Alice/paper-a")
                .unwrap()
                .unwrap()
                .profile_json["paper_key"],
            "Alice/paper-a"
        );
    }
}
