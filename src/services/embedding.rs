use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::storage::{SourceChunk, Storage};

#[derive(Debug, Clone)]
pub struct PendingChunkEmbedding {
    pub chunk: SourceChunk,
    pub chunk_hash: String,
}

pub struct EmbeddingService<'a> {
    storage: &'a Storage,
}

impl<'a> EmbeddingService<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    pub fn pending_chunks(
        &self,
        author: &str,
        limit: Option<usize>,
        model: &str,
        model_version: Option<&str>,
        force: bool,
    ) -> Result<Vec<PendingChunkEmbedding>> {
        let chunks = self.storage.all_chunks_for_author(author, limit)?;
        let mut pending = Vec::new();
        for chunk in chunks {
            let chunk_hash = chunk_hash(&chunk.text);
            if force
                || !self.storage.has_current_chunk_embedding(
                    chunk.id,
                    model,
                    model_version,
                    &chunk.source_hash,
                    &chunk_hash,
                )?
            {
                pending.push(PendingChunkEmbedding { chunk, chunk_hash });
            }
        }
        Ok(pending)
    }

    pub fn record_failure(
        &self,
        item: &PendingChunkEmbedding,
        model: &str,
        model_version: Option<&str>,
        error: &str,
    ) -> Result<()> {
        self.storage.record_embedding_job(
            &item.chunk,
            model,
            model_version,
            &item.chunk_hash,
            "failed",
            Some(error),
        )
    }

    pub fn save_success(
        &self,
        item: &PendingChunkEmbedding,
        model: &str,
        model_version: Option<&str>,
        vector: &[f32],
    ) -> Result<()> {
        self.storage.save_chunk_embedding(
            &item.chunk,
            model,
            model_version,
            vector,
            &item.chunk_hash,
        )?;
        self.storage.record_embedding_job(
            &item.chunk,
            model,
            model_version,
            &item.chunk_hash,
            "succeeded",
            None,
        )
    }
}

fn chunk_hash(text: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use super::EmbeddingService;
    use crate::papers::models::Paper;
    use crate::retrieval::chunker::chunk_paper;
    use crate::storage::Storage;

    #[test]
    fn pending_chunks_respects_current_embedding_and_force() {
        let dir = tempdir().unwrap();
        let mut storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: dir.path().to_path_buf(),
            article_path: dir.path().join("article.md"),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: BTreeMap::from([
                ("title".to_string(), "A Paper".to_string()),
                ("year".to_string(), "2024".to_string()),
            ]),
            fetch_result: json!({}),
            raw_body: String::new(),
            clean_text: "Catalyst conversion improved.".to_string(),
            sections: Vec::new(),
        };
        let chunks = chunk_paper(&paper, 3200, 350);
        storage.upsert_paper(&paper, &chunks).unwrap();
        let service = EmbeddingService::new(&storage);

        let pending = service
            .pending_chunks("Alice", None, "embed-model", Some("v1"), false)
            .unwrap();
        assert_eq!(pending.len(), 1);

        service
            .save_success(&pending[0], "embed-model", Some("v1"), &[0.1, 0.2])
            .unwrap();
        assert!(
            service
                .pending_chunks("Alice", None, "embed-model", Some("v1"), false)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            service
                .pending_chunks("Alice", None, "embed-model", Some("v1"), true)
                .unwrap()
                .len(),
            1
        );
    }
}
