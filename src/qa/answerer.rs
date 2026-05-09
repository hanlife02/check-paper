use anyhow::Result;

use crate::storage::Storage;
use crate::understanding::llm::OpenAiCompatibleClient;
use crate::understanding::prompts::qa_messages;

use super::planner::should_use_source_chunks;

pub struct Answerer<'a> {
    storage: &'a Storage,
    llm: OpenAiCompatibleClient,
}

impl<'a> Answerer<'a> {
    pub fn new(storage: &'a Storage, llm: OpenAiCompatibleClient) -> Self {
        Self { storage, llm }
    }

    pub fn answer(&self, author: &str, question: &str) -> Result<String> {
        let profiles = self.storage.search_profiles(author, question, 8)?;
        let mut chunks = Vec::new();
        if should_use_source_chunks(question, profiles.len()) {
            chunks = self.storage.search_chunks(author, question, 8)?;
        }

        let first = self
            .llm
            .chat(qa_messages(question, &profiles, &chunks), 0.2, 2200)?;
        if signals_insufficient(&first) && chunks.is_empty() {
            chunks = self.storage.search_chunks(author, question, 10)?;
            return self
                .llm
                .chat(qa_messages(question, &profiles, &chunks), 0.2, 2600);
        }
        Ok(first)
    }
}

fn signals_insufficient(answer: &str) -> bool {
    let lowered = answer.to_lowercase();
    lowered.contains("insufficient_context")
        || answer.contains("证据不足")
        || answer.contains("信息不足")
}
