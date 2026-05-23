use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use super::{AuthorProfileV2Record, NewAuthorProfileV2, Storage};

impl Storage {
    pub fn save_author_profile_v2(&self, profile: NewAuthorProfileV2<'_>) -> Result<bool> {
        let changed = self.author_profile_v2_changed(profile)?;
        self.conn.execute(
            r#"
            INSERT INTO author_profiles_v2 (
                author, profile_json, profile_schema_version, builder_version,
                model_id, source_profile_hash, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT(author) DO UPDATE SET
                profile_json = excluded.profile_json,
                profile_schema_version = excluded.profile_schema_version,
                builder_version = excluded.builder_version,
                model_id = excluded.model_id,
                source_profile_hash = excluded.source_profile_hash,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![
                profile.author,
                serde_json::to_string(profile.profile_json)?,
                profile.profile_schema_version,
                profile.builder_version,
                profile.model_id,
                profile.source_profile_hash,
            ],
        )?;
        Ok(changed)
    }

    pub fn author_profile_v2_is_current(
        &self,
        author: &str,
        profile_schema_version: i64,
        builder_version: &str,
        model_id: &str,
        source_profile_hash: &str,
    ) -> Result<bool> {
        let current: Option<i64> = self
            .conn
            .query_row(
                r#"
                SELECT 1
                FROM author_profiles_v2
                WHERE author = ?
                  AND profile_schema_version = ?
                  AND builder_version = ?
                  AND model_id = ?
                  AND source_profile_hash = ?
                "#,
                params![
                    author,
                    profile_schema_version,
                    builder_version,
                    model_id,
                    source_profile_hash
                ],
                |row| row.get(0),
            )
            .optional()?;
        Ok(current.is_some())
    }

    pub fn author_profile_v2(&self, author: &str) -> Result<Option<AuthorProfileV2Record>> {
        self.conn
            .query_row(
                r#"
                SELECT author, profile_json, profile_schema_version, builder_version,
                       model_id, source_profile_hash, updated_at
                FROM author_profiles_v2
                WHERE author = ?
                "#,
                params![author],
                author_profile_v2_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    fn author_profile_v2_changed(&self, profile: NewAuthorProfileV2<'_>) -> Result<bool> {
        let existing: Option<(String, i64, String, String, String)> = self
            .conn
            .query_row(
                r#"
                SELECT profile_json, profile_schema_version, builder_version, model_id,
                       source_profile_hash
                FROM author_profiles_v2
                WHERE author = ?
                "#,
                params![profile.author],
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
        let Some((profile_json, schema_version, builder_version, model_id, source_profile_hash)) =
            existing
        else {
            return Ok(true);
        };
        Ok(profile_json != serde_json::to_string(profile.profile_json)?
            || schema_version != profile.profile_schema_version
            || builder_version != profile.builder_version
            || model_id != profile.model_id
            || source_profile_hash != profile.source_profile_hash)
    }
}

fn author_profile_v2_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuthorProfileV2Record> {
    let profile_json: String = row.get(1)?;
    Ok(AuthorProfileV2Record {
        author: row.get(0)?,
        profile_json: serde_json::from_str::<Value>(&profile_json)
            .unwrap_or_else(|_| json!({ "raw_profile": profile_json })),
        profile_schema_version: row.get(2)?,
        builder_version: row.get(3)?,
        model_id: row.get(4)?,
        source_profile_hash: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn saves_and_checks_current_author_profile_v2() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let profile_json = json!({"author": "Alice"});
        let changed = storage
            .save_author_profile_v2(NewAuthorProfileV2 {
                author: "Alice",
                profile_json: &profile_json,
                profile_schema_version: 2,
                builder_version: "author-profile-v2-s4",
                model_id: "deterministic",
                source_profile_hash: "profiles",
            })
            .unwrap();

        assert!(changed);
        assert!(
            storage
                .author_profile_v2_is_current(
                    "Alice",
                    2,
                    "author-profile-v2-s4",
                    "deterministic",
                    "profiles"
                )
                .unwrap()
        );
        let unchanged = storage
            .save_author_profile_v2(NewAuthorProfileV2 {
                author: "Alice",
                profile_json: &profile_json,
                profile_schema_version: 2,
                builder_version: "author-profile-v2-s4",
                model_id: "deterministic",
                source_profile_hash: "profiles",
            })
            .unwrap();
        assert!(!unchanged);
        assert_eq!(
            storage
                .author_profile_v2("Alice")
                .unwrap()
                .unwrap()
                .profile_json["author"],
            "Alice"
        );
    }
}
