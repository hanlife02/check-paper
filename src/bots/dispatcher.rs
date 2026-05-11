use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchAction {
    Start {
        chat_id: i64,
        job_id: u64,
        text: String,
    },
    Queued {
        chat_id: i64,
        queue_len: usize,
    },
    Cancelled {
        chat_id: i64,
        active_job_id: Option<u64>,
    },
    NothingToCancel {
        chat_id: i64,
    },
}

#[derive(Debug, Default)]
pub struct ChatDispatcher {
    next_job_id: u64,
    active: HashMap<i64, ActiveJob>,
    queues: HashMap<i64, VecDeque<String>>,
}

#[derive(Debug)]
struct ActiveJob {
    id: u64,
}

impl ChatDispatcher {
    pub fn submit(&mut self, chat_id: i64, text: String) -> DispatchAction {
        if self.active.contains_key(&chat_id) {
            let queue = self.queues.entry(chat_id).or_default();
            queue.push_back(text);
            DispatchAction::Queued {
                chat_id,
                queue_len: queue.len(),
            }
        } else {
            self.next_job_id += 1;
            let job_id = self.next_job_id;
            self.active.insert(chat_id, ActiveJob { id: job_id });
            DispatchAction::Start {
                chat_id,
                job_id,
                text,
            }
        }
    }

    pub fn finish(&mut self, chat_id: i64) -> Option<DispatchAction> {
        self.active.remove(&chat_id);
        let next = self.queues.get_mut(&chat_id).and_then(VecDeque::pop_front);
        if let Some(text) = next {
            self.next_job_id += 1;
            let job_id = self.next_job_id;
            self.active.insert(chat_id, ActiveJob { id: job_id });
            Some(DispatchAction::Start {
                chat_id,
                job_id,
                text,
            })
        } else {
            None
        }
    }

    pub fn cancel(&mut self, chat_id: i64) -> DispatchAction {
        let active_job_id = self.active.remove(&chat_id).map(|job| job.id);
        let queued = self.queues.remove(&chat_id).map_or(0, |queue| queue.len());
        if active_job_id.is_some() || queued > 0 {
            DispatchAction::Cancelled {
                chat_id,
                active_job_id,
            }
        } else {
            DispatchAction::NothingToCancel { chat_id }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatDispatcher, DispatchAction};

    #[test]
    fn queues_messages_per_chat_but_allows_other_chats_to_start() {
        let mut dispatcher = ChatDispatcher::default();
        assert_eq!(
            dispatcher.submit(1, "first".to_string()),
            DispatchAction::Start {
                chat_id: 1,
                job_id: 1,
                text: "first".to_string()
            }
        );
        assert_eq!(
            dispatcher.submit(1, "second".to_string()),
            DispatchAction::Queued {
                chat_id: 1,
                queue_len: 1
            }
        );
        assert_eq!(
            dispatcher.submit(2, "other".to_string()),
            DispatchAction::Start {
                chat_id: 2,
                job_id: 2,
                text: "other".to_string()
            }
        );
        assert_eq!(
            dispatcher.finish(1),
            Some(DispatchAction::Start {
                chat_id: 1,
                job_id: 3,
                text: "second".to_string()
            })
        );
    }

    #[test]
    fn cancel_removes_active_and_queued_messages_for_chat() {
        let mut dispatcher = ChatDispatcher::default();
        dispatcher.submit(1, "first".to_string());
        dispatcher.submit(1, "second".to_string());
        assert_eq!(
            dispatcher.cancel(1),
            DispatchAction::Cancelled {
                chat_id: 1,
                active_job_id: Some(1)
            }
        );
        assert_eq!(dispatcher.finish(1), None);
        assert_eq!(
            dispatcher.cancel(1),
            DispatchAction::NothingToCancel { chat_id: 1 }
        );
    }
}
