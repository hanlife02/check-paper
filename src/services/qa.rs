use anyhow::{Result, anyhow};

use crate::qa::answerer::{Answerer, QaProfileVersion};
use crate::retrieval::embedding::OpenAiCompatibleEmbeddingClient;
use crate::storage::Storage;
use crate::understanding::llm::OpenAiCompatibleClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QaProfileVersionPreference {
    V1,
    V2,
    Auto,
}

impl QaProfileVersionPreference {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_lowercase().as_str() {
            "v1" => Ok(Self::V1),
            "v2" => Ok(Self::V2),
            "auto" => Ok(Self::Auto),
            value => Err(anyhow!(
                "invalid QA profile version `{value}`; expected v1, v2, or auto"
            )),
        }
    }

    pub fn resolve(self, storage: &Storage, author: &str) -> Result<QaProfileVersion> {
        match self {
            Self::V1 => Ok(QaProfileVersion::V1),
            Self::V2 => Ok(QaProfileVersion::V2),
            Self::Auto => {
                if storage
                    .paper_profiles_v2_for_author(author, Some(1))?
                    .is_empty()
                {
                    Ok(QaProfileVersion::V1)
                } else {
                    Ok(QaProfileVersion::V2)
                }
            }
        }
    }
}

pub struct QaService<'a> {
    answerer: Answerer<'a>,
}

impl<'a> QaService<'a> {
    pub fn new(
        storage: &'a Storage,
        llm: OpenAiCompatibleClient,
        embedding: Option<OpenAiCompatibleEmbeddingClient>,
    ) -> Self {
        Self::new_with_profile_version(storage, llm, embedding, QaProfileVersion::V1)
    }

    pub fn new_with_profile_version(
        storage: &'a Storage,
        llm: OpenAiCompatibleClient,
        embedding: Option<OpenAiCompatibleEmbeddingClient>,
        profile_version: QaProfileVersion,
    ) -> Self {
        Self {
            answerer: Answerer::new_with_embedding_and_profile_version(
                storage,
                llm,
                embedding,
                profile_version,
            ),
        }
    }

    pub fn new_with_profile_preference(
        storage: &'a Storage,
        llm: OpenAiCompatibleClient,
        embedding: Option<OpenAiCompatibleEmbeddingClient>,
        author: &str,
        profile_preference: QaProfileVersionPreference,
    ) -> Result<Self> {
        Ok(Self::new_with_profile_version(
            storage,
            llm,
            embedding,
            profile_preference.resolve(storage, author)?,
        ))
    }

    pub fn answer(&self, author: &str, question: &str) -> Result<String> {
        self.answerer.answer(author, question)
    }

    pub async fn answer_stream<F>(
        &self,
        author: &str,
        question: &str,
        on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        self.answerer
            .answer_stream(author, question, on_delta)
            .await
    }

    pub async fn answer_stream_with_telegram_context<F>(
        &self,
        author: &str,
        question: &str,
        telegram_chat_id: i64,
        telegram_job_id: i64,
        on_delta: F,
    ) -> Result<String>
    where
        F: FnMut(&str) -> Result<()>,
    {
        self.answerer
            .answer_stream_with_telegram_context(
                author,
                question,
                telegram_chat_id,
                telegram_job_id,
                on_delta,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use super::QaProfileVersionPreference;
    use crate::papers::models::Paper;
    use crate::qa::answerer::QaProfileVersion;
    use crate::storage::{NewPaperProfileV2, Storage};

    #[test]
    fn parses_profile_version_preference() {
        assert_eq!(
            QaProfileVersionPreference::parse("v1").unwrap(),
            QaProfileVersionPreference::V1
        );
        assert_eq!(
            QaProfileVersionPreference::parse("AUTO").unwrap(),
            QaProfileVersionPreference::Auto
        );
        assert!(QaProfileVersionPreference::parse("v3").is_err());
    }

    #[test]
    fn auto_profile_version_uses_v2_when_author_has_v2_profiles() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .upsert_paper(&test_paper(dir.path(), "Alice", "paper-a"), &[])
            .unwrap();

        assert_eq!(
            QaProfileVersionPreference::Auto
                .resolve(&storage, "Alice")
                .unwrap(),
            QaProfileVersion::V1
        );

        storage
            .save_paper_profile_v2(NewPaperProfileV2 {
                paper_key: "Alice/paper-a",
                profile_json: &json!({
                    "paper_key": "Alice/paper-a",
                    "title": "A Paper",
                    "one_sentence_summary": "A supported V2 summary.",
                    "factual_objects": []
                }),
                profile_schema_version: 2,
                builder_version: "test-builder",
                model_id: "test-model",
                source_fact_hash: "facts-a",
            })
            .unwrap();

        assert_eq!(
            QaProfileVersionPreference::Auto
                .resolve(&storage, "Alice")
                .unwrap(),
            QaProfileVersion::V2
        );
    }

    fn test_paper(root: &std::path::Path, author: &str, paper_id: &str) -> Paper {
        Paper {
            author: author.to_string(),
            paper_id: paper_id.to_string(),
            paper_dir: root.join(author).join(paper_id),
            article_path: root.join(author).join(paper_id).join("article.md"),
            fetch_result_path: None,
            source_hash: format!("{author}-{paper_id}-hash"),
            metadata: BTreeMap::from([
                ("title".to_string(), "A Paper".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![],
        }
    }
}
