use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};

use crate::retrieval::embedding::EmbeddingProvider;
use crate::storage::{SourceChunk, Storage};

#[derive(Debug, Clone)]
pub struct PendingChunkEmbedding {
    pub chunk: SourceChunk,
    pub chunk_hash: String,
}

#[derive(Debug, Clone, Copy)]
pub struct EmbeddingRunOptions {
    pub limit: Option<usize>,
    pub force: bool,
    pub max_attempts: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingRunReport {
    pub model: String,
    pub model_version: Option<String>,
    pub pending: usize,
    pub embedded: usize,
    pub failed: usize,
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

    pub fn embed_author<C: EmbeddingProvider>(
        &self,
        author: &str,
        client: &C,
        options: EmbeddingRunOptions,
    ) -> Result<EmbeddingRunReport> {
        let pending = self.pending_chunks(
            author,
            options.limit,
            client.model_name(),
            client.model_version(),
            options.force,
        )?;
        let mut report = EmbeddingRunReport {
            model: client.model_name().to_string(),
            model_version: client.model_version().map(str::to_string),
            pending: pending.len(),
            embedded: 0,
            failed: 0,
        };
        for batch in pending.chunks(client.batch_size()) {
            let input = batch
                .iter()
                .map(embedding_input_for_chunk)
                .collect::<Vec<_>>();
            let vectors = match embed_batch_with_retries(client, &input, options.max_attempts) {
                Ok(vectors) => vectors,
                Err(err) => {
                    for item in batch {
                        self.record_failure(
                            item,
                            client.model_name(),
                            client.model_version(),
                            &err.to_string(),
                        )?;
                    }
                    report.failed += batch.len();
                    return Err(err);
                }
            };
            if vectors.len() != batch.len() {
                let err = anyhow!(
                    "embedding provider returned {} vectors for {} inputs",
                    vectors.len(),
                    batch.len()
                );
                for item in batch {
                    self.record_failure(
                        item,
                        client.model_name(),
                        client.model_version(),
                        &err.to_string(),
                    )?;
                }
                report.failed += batch.len();
                return Err(err);
            }
            for (item, vector) in batch.iter().zip(vectors.iter()) {
                self.save_success(item, client.model_name(), client.model_version(), vector)?;
                report.embedded += 1;
            }
        }
        Ok(report)
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

fn embedding_input_for_chunk(item: &PendingChunkEmbedding) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        item.chunk.title, item.chunk.doi, item.chunk.section, item.chunk.text
    )
}

fn embed_batch_with_retries<C: EmbeddingProvider>(
    client: &C,
    input: &[String],
    max_attempts: usize,
) -> Result<Vec<Vec<f32>>> {
    let max_attempts = max_attempts.max(1);
    let mut last_error = None;
    for _attempt in 1..=max_attempts {
        match client.embed(input) {
            Ok(vectors) => return Ok(vectors),
            Err(err) => last_error = Some(err),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("embedding failed")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::tempdir;

    use anyhow::Result;

    use super::{EmbeddingRunOptions, EmbeddingService};
    use crate::papers::models::Paper;
    use crate::retrieval::chunker::chunk_paper;
    use crate::retrieval::embedding::EmbeddingProvider;
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

    #[test]
    fn embed_author_saves_successes_with_provider() {
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

        let report = service
            .embed_author(
                "Alice",
                &FakeEmbeddingProvider,
                EmbeddingRunOptions {
                    limit: None,
                    force: false,
                    max_attempts: 1,
                },
            )
            .unwrap();

        assert_eq!(report.pending, 1);
        assert_eq!(report.embedded, 1);
        assert_eq!(report.failed, 0);
        assert!(
            service
                .pending_chunks("Alice", None, "fake-embedding", Some("v1"), false)
                .unwrap()
                .is_empty()
        );
    }

    struct FakeEmbeddingProvider;

    impl EmbeddingProvider for FakeEmbeddingProvider {
        fn model_name(&self) -> &str {
            "fake-embedding"
        }

        fn model_version(&self) -> Option<&str> {
            Some("v1")
        }

        fn batch_size(&self) -> usize {
            8
        }

        fn embed(&self, input: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(input.iter().map(|_| vec![0.1, 0.2]).collect())
        }
    }
}
