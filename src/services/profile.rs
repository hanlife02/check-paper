use anyhow::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::schemas::author_profile::AUTHOR_PROFILE_SCHEMA_VERSION;
use crate::storage::Storage;
use crate::understanding::author_analyzer::build_author_profile;
use crate::understanding::llm::OpenAiCompatibleClient;
use crate::understanding::prompts::AUTHOR_PROFILE_PROMPT_VERSION;

pub enum AuthorProfileLookup {
    Found(Value),
    Missing { paper_count: i64 },
}

pub enum AuthorProfileRebuild {
    NoPaperProfiles,
    Current { profile_count: usize },
    Rebuilt { profile_count: usize },
}

pub struct ProfileService<'a> {
    storage: &'a Storage,
}

impl<'a> ProfileService<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    pub fn author_profile(&self, author: &str) -> Result<AuthorProfileLookup> {
        if let Some(profile) = self.storage.get_author_profile(author)? {
            Ok(AuthorProfileLookup::Found(profile))
        } else {
            Ok(AuthorProfileLookup::Missing {
                paper_count: self.storage.count_papers(Some(author))?,
            })
        }
    }

    pub fn rebuild_author_profile(
        &self,
        author: &str,
        llm: &OpenAiCompatibleClient,
        force: bool,
    ) -> Result<AuthorProfileRebuild> {
        let profiles = self.storage.paper_profiles(author, None)?;
        if profiles.is_empty() {
            return Ok(AuthorProfileRebuild::NoPaperProfiles);
        }
        let source_profile_hash = hash_text(&serde_json::to_string(&profiles)?);
        if !force
            && self.storage.author_profile_is_current(
                author,
                AUTHOR_PROFILE_SCHEMA_VERSION,
                AUTHOR_PROFILE_PROMPT_VERSION,
                llm.model_name(),
                &source_profile_hash,
            )?
        {
            return Ok(AuthorProfileRebuild::Current {
                profile_count: profiles.len(),
            });
        }
        let author_profile = build_author_profile(author, &profiles, Some(llm))?;
        self.storage.save_author_profile_with_metadata(
            author,
            &author_profile,
            AUTHOR_PROFILE_SCHEMA_VERSION,
            AUTHOR_PROFILE_PROMPT_VERSION,
            llm.model_name(),
            &source_profile_hash,
        )?;
        Ok(AuthorProfileRebuild::Rebuilt {
            profile_count: profiles.len(),
        })
    }
}

fn hash_text(text: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::{AuthorProfileLookup, AuthorProfileRebuild, ProfileService};
    use crate::storage::Storage;
    use crate::understanding::llm::{LlmConfig, OpenAiCompatibleClient};

    #[test]
    fn distinguishes_found_and_missing_author_profiles() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .save_author_profile("Alice", &json!({ "author": "Alice" }))
            .unwrap();
        let service = ProfileService::new(&storage);

        match service.author_profile("Alice").unwrap() {
            AuthorProfileLookup::Found(profile) => assert_eq!(profile["author"], "Alice"),
            AuthorProfileLookup::Missing { .. } => panic!("expected author profile"),
        }
        match service.author_profile("Bob").unwrap() {
            AuthorProfileLookup::Missing { paper_count } => assert_eq!(paper_count, 0),
            AuthorProfileLookup::Found(_) => panic!("expected missing author profile"),
        }
    }

    #[test]
    fn rebuild_reports_no_paper_profiles_without_calling_llm() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let llm = OpenAiCompatibleClient::new(LlmConfig {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            api_key: None,
            model: "test-model".to_string(),
            proxy: None,
            timeout_secs: 1,
            tls_backend: "rustls".to_string(),
            prompt_cost_per_1k: None,
            completion_cost_per_1k: None,
        })
        .unwrap();

        match ProfileService::new(&storage)
            .rebuild_author_profile("Alice", &llm, true)
            .unwrap()
        {
            AuthorProfileRebuild::NoPaperProfiles => {}
            _ => panic!("expected no paper profiles"),
        }
    }
}
