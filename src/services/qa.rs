use anyhow::Result;

use crate::qa::answerer::Answerer;
use crate::retrieval::embedding::OpenAiCompatibleEmbeddingClient;
use crate::storage::Storage;
use crate::understanding::llm::OpenAiCompatibleClient;

pub struct QaService<'a> {
    answerer: Answerer<'a>,
}

impl<'a> QaService<'a> {
    pub fn new(
        storage: &'a Storage,
        llm: OpenAiCompatibleClient,
        embedding: Option<OpenAiCompatibleEmbeddingClient>,
    ) -> Self {
        Self {
            answerer: Answerer::new_with_embedding(storage, llm, embedding),
        }
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
}
