const SOURCE_DETAIL_KEYWORDS: &[&str] = &[
    "原文", "引用", "证据", "具体", "数值", "多少", "性能", "实验", "条件", "机制", "比较", "对比",
    "figure", "table", "doi", "数据",
];

pub fn should_use_source_chunks(question: &str, profile_count: usize) -> bool {
    if profile_count == 0 {
        return true;
    }
    let lowered = question.to_lowercase();
    SOURCE_DETAIL_KEYWORDS
        .iter()
        .any(|keyword| lowered.contains(keyword))
}
