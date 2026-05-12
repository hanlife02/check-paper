use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use super::Storage;

impl Storage {
    pub fn save_paper_profile(
        &self,
        paper_key: &str,
        source_hash: &str,
        profile: &Value,
    ) -> Result<()> {
        self.save_paper_profile_with_metadata(paper_key, source_hash, profile, 1, "", "", "")
    }

    pub fn save_paper_profile_with_metadata(
        &self,
        paper_key: &str,
        source_hash: &str,
        profile: &Value,
        profile_schema_version: i64,
        profile_prompt_version: &str,
        profile_model_id: &str,
        profile_chunker_version: &str,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            UPDATE papers
            SET profile_json = ?, analyzed_hash = ?, analyzed_at = CURRENT_TIMESTAMP,
                profile_generated_at = CURRENT_TIMESTAMP,
                profile_schema_version = ?,
                profile_prompt_version = ?,
                profile_model_id = ?,
                profile_chunker_version = ?,
                profile_status = 'succeeded',
                profile_error_code = NULL
            WHERE paper_key = ?
            "#,
            params![
                serde_json::to_string(profile)?,
                source_hash,
                profile_schema_version,
                profile_prompt_version,
                profile_model_id,
                profile_chunker_version,
                paper_key
            ],
        )?;
        self.save_profile_facts(paper_key, profile, profile_schema_version)?;
        Ok(())
    }

    pub fn paper_profiles(&self, author: &str, limit: Option<usize>) -> Result<Vec<Value>> {
        let mut sql = r#"
            SELECT paper_key, title, doi, year, profile_json
            FROM papers
            WHERE author = ? AND profile_json IS NOT NULL
            ORDER BY year DESC, title ASC
        "#
        .to_string();
        if limit.is_some() {
            sql.push_str(" LIMIT ?");
        }

        let mut profiles = Vec::new();
        if let Some(limit) = limit {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![author, limit as i64], parse_profile_row)?;
            for row in rows {
                profiles.push(row?);
            }
        } else {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(params![author], parse_profile_row)?;
            for row in rows {
                profiles.push(row?);
            }
        }
        Ok(profiles)
    }

    pub fn save_author_profile(&self, author: &str, profile: &Value) -> Result<()> {
        self.save_author_profile_with_metadata(author, profile, 1, "", "", "")
    }

    pub fn save_author_profile_with_metadata(
        &self,
        author: &str,
        profile: &Value,
        profile_schema_version: i64,
        profile_prompt_version: &str,
        profile_model_id: &str,
        source_profile_hash: &str,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO author_profiles (
                author, profile_json, profile_schema_version, profile_prompt_version,
                profile_model_id, source_profile_hash, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(author) DO UPDATE SET
                profile_json = excluded.profile_json,
                profile_schema_version = excluded.profile_schema_version,
                profile_prompt_version = excluded.profile_prompt_version,
                profile_model_id = excluded.profile_model_id,
                source_profile_hash = excluded.source_profile_hash,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![
                author,
                serde_json::to_string(profile)?,
                profile_schema_version,
                profile_prompt_version,
                profile_model_id,
                source_profile_hash
            ],
        )?;
        Ok(())
    }

    pub fn get_author_profile(&self, author: &str) -> Result<Option<Value>> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT profile_json FROM author_profiles WHERE author = ?",
                params![author],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.and_then(|text| serde_json::from_str(&text).ok()))
    }

    pub fn author_profile_is_current(
        &self,
        author: &str,
        profile_schema_version: i64,
        profile_prompt_version: &str,
        profile_model_id: &str,
        source_profile_hash: &str,
    ) -> Result<bool> {
        let metadata: Option<(i64, String, String, String)> = self
            .conn
            .query_row(
                r#"
                SELECT COALESCE(profile_schema_version, 0),
                       COALESCE(profile_prompt_version, ''),
                       COALESCE(profile_model_id, ''),
                       COALESCE(source_profile_hash, '')
                FROM author_profiles
                WHERE author = ?
                "#,
                params![author],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        Ok(
            metadata.is_some_and(|(schema_version, prompt_version, model_id, stored_hash)| {
                schema_version == profile_schema_version
                    && prompt_version == profile_prompt_version
                    && model_id == profile_model_id
                    && stored_hash == source_profile_hash
            }),
        )
    }
}

fn parse_profile_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let paper_key: String = row.get(0)?;
    let title: String = row.get(1)?;
    let doi: String = row.get(2)?;
    let year: String = row.get(3)?;
    let profile_json: String = row.get(4)?;
    let mut profile: Value = serde_json::from_str(&profile_json)
        .unwrap_or_else(|_| json!({ "raw_profile": profile_json }));
    if let Some(object) = profile.as_object_mut() {
        object
            .entry("paper_key")
            .or_insert(Value::String(paper_key));
        object.entry("title").or_insert(Value::String(title));
        object.entry("doi").or_insert(Value::String(doi));
        object.entry("year").or_insert(Value::String(year));
    }
    Ok(profile)
}
