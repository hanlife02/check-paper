use anyhow::Result;
use serde_json::Value;

use crate::storage::Storage;

pub struct SourcesService<'a> {
    storage: &'a Storage,
}

impl<'a> SourcesService<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    pub fn latest_answer(&self, author: Option<&str>) -> Result<Option<Value>> {
        self.storage.latest_qa_answer(author)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::SourcesService;
    use crate::storage::Storage;

    #[test]
    fn returns_latest_answer_for_author() {
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.sqlite")).unwrap();
        storage
            .save_qa_log(
                "Alice",
                "question",
                &json!({ "chunks": [] }),
                &json!({ "answer": "ok" }),
                "test-model",
                12,
            )
            .unwrap();
        let answer = SourcesService::new(&storage)
            .latest_answer(Some("Alice"))
            .unwrap()
            .unwrap();

        assert_eq!(answer["answer"], "ok");
    }
}
