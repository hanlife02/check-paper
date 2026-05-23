const SOURCE_DETAIL_KEYWORDS: &[&str] = &[
    "原文",
    "引用",
    "证据",
    "具体",
    "数值",
    "多少",
    "性能",
    "实验",
    "条件",
    "机制",
    "比较",
    "对比",
    "figure",
    "table",
    "doi",
    "数据",
    "which paper",
    "which 2026",
    "which review",
    "which perspective",
    "which strategy",
    "what lattice",
    "what charge",
    "what latent",
    "how does the",
    "claim",
    "connect",
    "reports",
    "reported",
    "uses ",
    "proposes",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QaRoutePlan {
    pub qa_mode: &'static str,
    pub route_reason: &'static str,
    pub use_source_chunks: bool,
}

pub fn plan_qa_route(question: &str, profile_count: usize) -> QaRoutePlan {
    if profile_count == 0 {
        return QaRoutePlan {
            qa_mode: "source_evidence",
            route_reason: "profile_missing",
            use_source_chunks: true,
        };
    }
    let lowered = question.to_lowercase();
    if SOURCE_DETAIL_KEYWORDS
        .iter()
        .any(|keyword| lowered.contains(keyword))
    {
        return QaRoutePlan {
            qa_mode: "source_evidence",
            route_reason: "detail_keyword",
            use_source_chunks: true,
        };
    }
    QaRoutePlan {
        qa_mode: "profile_first",
        route_reason: "broad_profile_context",
        use_source_chunks: false,
    }
}

pub fn should_use_source_chunks(question: &str, profile_count: usize) -> bool {
    plan_qa_route(question, profile_count).use_source_chunks
}

#[cfg(test)]
mod tests {
    use super::{plan_qa_route, should_use_source_chunks};

    #[test]
    fn routes_broad_profile_questions_profile_first() {
        let plan = plan_qa_route("这些论文主要讲什么？", 3);

        assert_eq!(plan.qa_mode, "profile_first");
        assert_eq!(plan.route_reason, "broad_profile_context");
        assert!(!plan.use_source_chunks);
        assert!(!should_use_source_chunks("这些论文主要讲什么？", 3));
    }

    #[test]
    fn routes_detail_questions_to_source_evidence() {
        let plan = plan_qa_route("paper-a 的实验条件是什么？", 3);

        assert_eq!(plan.qa_mode, "source_evidence");
        assert_eq!(plan.route_reason, "detail_keyword");
        assert!(plan.use_source_chunks);
    }

    #[test]
    fn routes_english_paper_identification_to_source_evidence() {
        let plan = plan_qa_route("Which 2026 paper reports smart garments?", 3);

        assert_eq!(plan.qa_mode, "source_evidence");
        assert_eq!(plan.route_reason, "detail_keyword");
        assert!(plan.use_source_chunks);
    }

    #[test]
    fn routes_without_profiles_to_source_evidence() {
        let plan = plan_qa_route("这些论文主要讲什么？", 0);

        assert_eq!(plan.qa_mode, "source_evidence");
        assert_eq!(plan.route_reason, "profile_missing");
        assert!(plan.use_source_chunks);
    }
}
