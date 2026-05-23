use std::collections::BTreeSet;

use super::super::papers::models::{Paper, Section};

#[derive(Debug, Clone)]
pub struct Chunk {
    pub paper_key: String,
    pub chunk_index: usize,
    pub section: String,
    pub section_kind: String,
    pub caption_label: Option<String>,
    pub caption_object_type: Option<String>,
    pub caption_object_label: Option<String>,
    pub caption_panel_labels_json: Option<String>,
    pub caption_target_labels_json: Option<String>,
    pub caption_panel_details_json: Option<String>,
    pub caption_measurements_json: Option<String>,
    pub caption_conditions_json: Option<String>,
    pub caption_values_json: Option<String>,
    pub text: String,
}

pub fn chunk_paper(paper: &Paper, max_chars: usize, overlap: usize) -> Vec<Chunk> {
    let fallback;
    let sections = if paper.sections.is_empty() {
        fallback = vec![Section {
            title: "Body".to_string(),
            level: 1,
            content: paper.clean_text.clone(),
        }];
        &fallback
    } else {
        &paper.sections
    };

    let mut chunks = Vec::new();
    for section in sections {
        let section_kind = section.section_kind().to_string();
        let caption_label = section.caption_label();
        let caption_metadata = section.caption_metadata();
        let inferred_panel_labels =
            if section_kind == "figure_caption" || section_kind == "table_caption" {
                caption_panel_labels_from_text(&section.content)
            } else {
                Vec::new()
            };
        let caption_panel_labels = caption_metadata
            .as_ref()
            .map(|metadata| {
                if metadata.panel_labels.is_empty() && !inferred_panel_labels.is_empty() {
                    inferred_panel_labels.clone()
                } else {
                    metadata.panel_labels.clone()
                }
            })
            .unwrap_or_default();
        let caption_target_labels = caption_metadata
            .as_ref()
            .map(|metadata| {
                if metadata.panel_labels.is_empty() && !inferred_panel_labels.is_empty() {
                    caption_targets_for_inferred_panels(
                        caption_label.as_deref(),
                        &metadata.object_label,
                        &inferred_panel_labels,
                    )
                } else {
                    metadata.target_labels.clone()
                }
            })
            .unwrap_or_default();
        let caption_panel_labels_json = non_empty_json_array(&caption_panel_labels);
        let caption_target_labels_json = non_empty_json_array(&caption_target_labels);
        let caption_panel_details_json =
            if section_kind == "figure_caption" || section_kind == "table_caption" {
                caption_panel_details_from_text(
                    &section.content,
                    &caption_panel_labels,
                    &caption_target_labels,
                )
            } else {
                None
            };
        let caption_details = if section_kind == "figure_caption" || section_kind == "table_caption"
        {
            Some(caption_detail_metadata(&section.content))
        } else {
            None
        };
        let caption_measurements_json = caption_details
            .as_ref()
            .and_then(|metadata| non_empty_json_array(&metadata.measurements));
        let caption_conditions_json = caption_details
            .as_ref()
            .and_then(|metadata| non_empty_json_array(&metadata.conditions));
        let caption_values_json = caption_details
            .as_ref()
            .and_then(|metadata| non_empty_json_array(&metadata.values));
        for piece in split_text(&section.content, max_chars, overlap) {
            chunks.push(Chunk {
                paper_key: paper.key(),
                chunk_index: chunks.len(),
                section: section.title.clone(),
                section_kind: section_kind.clone(),
                caption_label: caption_label.clone(),
                caption_object_type: caption_metadata
                    .as_ref()
                    .map(|metadata| metadata.object_type.clone()),
                caption_object_label: caption_metadata
                    .as_ref()
                    .map(|metadata| metadata.object_label.clone()),
                caption_panel_labels_json: caption_panel_labels_json.clone(),
                caption_target_labels_json: caption_target_labels_json.clone(),
                caption_panel_details_json: caption_panel_details_json.clone(),
                caption_measurements_json: caption_measurements_json.clone(),
                caption_conditions_json: caption_conditions_json.clone(),
                caption_values_json: caption_values_json.clone(),
                text: piece,
            });
        }
    }
    chunks
}

#[derive(Debug, Default)]
struct CaptionDetailMetadata {
    measurements: Vec<String>,
    conditions: Vec<String>,
    values: Vec<String>,
}

fn caption_detail_metadata(text: &str) -> CaptionDetailMetadata {
    let text = text.split_once(':').map(|(_, tail)| tail).unwrap_or(text);
    let tokens = caption_tokens(text);
    let mut measurements = Vec::new();
    let mut conditions = Vec::new();
    let mut values = Vec::new();
    let mut seen_measurements = BTreeSet::new();
    let mut seen_conditions = BTreeSet::new();
    let mut seen_values = BTreeSet::new();

    for (index, token) in tokens.iter().enumerate() {
        if is_numeric_token(token) {
            push_unique(&mut values, &mut seen_values, token);
            let measurement = measurement_from_tokens(&tokens, index);
            if let Some(named_measurement) = named_measurement_from_tokens(
                &tokens,
                index,
                measurement.as_deref().unwrap_or(token),
            ) {
                push_unique(
                    &mut measurements,
                    &mut seen_measurements,
                    &named_measurement,
                );
            }
            if let Some(measurement) = measurement {
                push_unique(&mut measurements, &mut seen_measurements, &measurement);
            }
        }
        if is_condition_starter(token) {
            let condition = condition_from_tokens(&tokens, index);
            if condition.split_whitespace().count() > 1 {
                push_unique(&mut conditions, &mut seen_conditions, &condition);
            }
        }
    }

    CaptionDetailMetadata {
        measurements,
        conditions,
        values,
    }
}

fn caption_panel_labels_from_text(text: &str) -> Vec<String> {
    let mut labels = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, ch) in text.char_indices() {
        if ch != '(' {
            continue;
        }
        let after_open = &text[index + ch.len_utf8()..];
        let Some(close_index) = after_open.find(')') else {
            continue;
        };
        if close_index > 5 {
            continue;
        }
        for label in panel_marker_labels(&after_open[..close_index]) {
            push_unique(&mut labels, &mut seen, &label);
        }
    }
    labels
}

fn caption_panel_details_from_text(
    text: &str,
    panel_labels: &[String],
    target_labels: &[String],
) -> Option<String> {
    let mut details = Vec::new();
    for (labels, description) in caption_panel_segments(text) {
        let metadata = caption_detail_metadata(&description);
        let relations =
            caption_panel_relations_from_text(&description, panel_labels, target_labels);
        let relation_paths = caption_relation_paths_from_relations(&relations);
        let cross_references = caption_cross_references_from_text(&description, target_labels);
        for label in labels {
            let target_label = panel_labels
                .iter()
                .position(|candidate| candidate == &label)
                .and_then(|index| target_labels.get(index))
                .cloned()
                .unwrap_or_else(|| label.clone());
            let mut detail = serde_json::json!({
                "panel_label": label,
                "target_label": target_label,
                "description": description,
                "measurements": metadata.measurements.clone(),
                "conditions": metadata.conditions.clone(),
                "values": metadata.values.clone(),
            });
            if !relations.is_empty()
                && let Some(object) = detail.as_object_mut()
            {
                object.insert(
                    "relations".to_string(),
                    serde_json::Value::Array(relations.clone()),
                );
            }
            if !relation_paths.is_empty()
                && let Some(object) = detail.as_object_mut()
            {
                object.insert(
                    "relation_paths".to_string(),
                    serde_json::Value::Array(relation_paths.clone()),
                );
            }
            if !cross_references.is_empty()
                && let Some(object) = detail.as_object_mut()
            {
                object.insert(
                    "cross_references".to_string(),
                    serde_json::Value::Array(cross_references.clone()),
                );
            }
            details.push(detail);
        }
    }
    if details.is_empty() {
        None
    } else {
        serde_json::to_string(&details).ok()
    }
}

fn caption_panel_relations_from_text(
    text: &str,
    panel_labels: &[String],
    target_labels: &[String],
) -> Vec<serde_json::Value> {
    let evidence = clean_panel_description(text);
    if evidence.is_empty() || panel_labels.len() < 2 {
        return Vec::new();
    }
    let mut relations = Vec::new();
    let mut seen = BTreeSet::new();
    for subject in panel_labels {
        for object in panel_labels {
            if subject == object {
                continue;
            }
            let Some(relation) = relation_between_panels(text, subject, object, panel_labels)
            else {
                continue;
            };
            let key = format!("{subject}\t{relation}\t{object}");
            if !seen.insert(key) {
                continue;
            }
            relations.push(serde_json::json!({
                "subject_panel_label": subject,
                "subject_target_label": target_label_for_panel(subject, panel_labels, target_labels),
                "relation": relation,
                "object_panel_label": object,
                "object_target_label": target_label_for_panel(object, panel_labels, target_labels),
                "evidence": evidence,
            }));
        }
    }
    relations
}

fn caption_relation_paths_from_relations(
    relations: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let mut paths = Vec::new();
    let mut seen = BTreeSet::new();
    for first in relations {
        for second in relations {
            let Some(first_subject_panel) = relation_str(first, "subject_panel_label") else {
                continue;
            };
            let Some(first_subject_target) = relation_str(first, "subject_target_label") else {
                continue;
            };
            let Some(first_relation) = relation_str(first, "relation") else {
                continue;
            };
            let Some(via_panel) = relation_str(first, "object_panel_label") else {
                continue;
            };
            let Some(via_target) = relation_str(first, "object_target_label") else {
                continue;
            };
            let Some(second_subject_panel) = relation_str(second, "subject_panel_label") else {
                continue;
            };
            if via_panel != second_subject_panel {
                continue;
            }
            let Some(second_relation) = relation_str(second, "relation") else {
                continue;
            };
            let Some(end_panel) = relation_str(second, "object_panel_label") else {
                continue;
            };
            if first_subject_panel == end_panel {
                continue;
            }
            let Some(end_target) = relation_str(second, "object_target_label") else {
                continue;
            };
            let evidence = relation_str(first, "evidence").unwrap_or_default();
            let key = format!(
                "{first_subject_panel}\t{first_relation}\t{via_panel}\t{second_relation}\t{end_panel}"
            );
            if !seen.insert(key) {
                continue;
            }
            paths.push(serde_json::json!({
                "start_panel_label": first_subject_panel,
                "start_target_label": first_subject_target,
                "via_panel_label": via_panel,
                "via_target_label": via_target,
                "end_panel_label": end_panel,
                "end_target_label": end_target,
                "relations": [first_relation, second_relation],
                "evidence": evidence,
            }));
        }
    }
    paths
}

fn relation_str<'a>(relation: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    relation.get(field)?.as_str()
}

fn caption_cross_references_from_text(
    text: &str,
    current_target_labels: &[String],
) -> Vec<serde_json::Value> {
    let evidence = clean_panel_description(text);
    if evidence.is_empty() {
        return Vec::new();
    }
    let current_targets = current_target_labels
        .iter()
        .map(|label| normalized_target_reference(label))
        .collect::<BTreeSet<_>>();
    let tokens = caption_token_spans(text);
    let mut references = Vec::new();
    let mut seen = BTreeSet::new();

    for (index, token) in tokens.iter().enumerate() {
        let Some(prefix) = caption_reference_prefix(&token.value) else {
            continue;
        };
        let Some(label) = tokens.get(index + 1).and_then(|next| {
            if is_caption_object_reference_label(&next.value) {
                Some(
                    next.value
                        .trim_matches(|ch: char| matches!(ch, ',' | ';'))
                        .to_string(),
                )
            } else {
                None
            }
        }) else {
            continue;
        };
        let target_label = format!("{prefix} {label}");
        let normalized_target = normalized_target_reference(&target_label);
        if current_targets.contains(&normalized_target) || !seen.insert(normalized_target) {
            continue;
        }
        let relation_context = reference_relation_context(text, token.start, tokens[index + 1].end);
        references.push(serde_json::json!({
            "target_label": target_label,
            "relation": cross_reference_relation(relation_context),
            "evidence": evidence,
        }));
    }

    references
}

fn reference_relation_context(text: &str, reference_start: usize, reference_end: usize) -> &str {
    let start = text[..reference_start]
        .rfind([';', ',', '.', ':', '\n'])
        .map(|index| index + 1)
        .unwrap_or(0);
    &text[start..reference_end]
}

fn cross_reference_relation(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    [
        (
            "caused_by",
            [
                "caused by",
                "driven by",
                "enabled by",
                "induced by",
                "triggered by",
                "promoted by",
                "due to",
                "attributed to",
            ]
            .as_slice(),
        ),
        (
            "inhibited_by",
            [
                "blocked by",
                "inhibited by",
                "prevented by",
                "suppressed by",
            ]
            .as_slice(),
        ),
        (
            "compared_with",
            [
                "compared with",
                "compared to",
                "relative to",
                "versus",
                "vs",
            ]
            .as_slice(),
        ),
        (
            "summarized_in",
            [
                "summarized in",
                "summarised in",
                "summary in",
                "shown in",
                "listed in",
                "tabulated in",
                "reported in",
            ]
            .as_slice(),
        ),
        (
            "derived_from",
            [
                "derived from",
                "adapted from",
                "reproduced from",
                "based on",
                "follows",
            ]
            .as_slice(),
        ),
    ]
    .iter()
    .flat_map(|(relation, phrases)| {
        phrases
            .iter()
            .filter_map(|phrase| lower.rfind(phrase).map(|index| (index, *relation)))
    })
    .max_by_key(|(index, _)| *index)
    .map(|(_, relation)| relation)
    .unwrap_or("references")
}

fn caption_reference_prefix(token: &str) -> Option<&'static str> {
    match token
        .trim_end_matches('.')
        .trim_matches(|ch: char| matches!(ch, '(' | ')' | '[' | ']'))
        .to_lowercase()
        .as_str()
    {
        "fig" | "figure" => Some("Figure"),
        "table" => Some("Table"),
        _ => None,
    }
}

fn is_caption_object_reference_label(token: &str) -> bool {
    let token = token.trim_matches(|ch: char| matches!(ch, ',' | ';' | ')' | ']' | '.'));
    token.chars().any(|ch| ch.is_ascii_digit())
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '‐' | '‑' | '–' | '—'))
}

fn normalized_target_reference(target: &str) -> String {
    target
        .to_lowercase()
        .replace("fig.", "figure")
        .replace("fig ", "figure ")
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

fn relation_between_panels(
    text: &str,
    subject: &str,
    object: &str,
    panel_labels: &[String],
) -> Option<&'static str> {
    let subject_positions = panel_reference_positions(text, subject);
    let object_positions = panel_reference_positions(text, object);
    for subject_position in &subject_positions {
        for object_position in &object_positions {
            if subject_position.end > object_position.start {
                continue;
            }
            if has_intervening_panel_reference(
                text,
                subject_position.end,
                object_position.start,
                subject,
                object,
                panel_labels,
            ) {
                continue;
            }
            let between = text[subject_position.end..object_position.start].to_lowercase();
            if between.contains("causes")
                || between.contains("caused")
                || between.contains("drives")
                || between.contains("driven")
                || between.contains("enables")
                || between.contains("induces")
                || between.contains("induced")
                || between.contains("leads to")
                || between.contains("promotes")
                || between.contains("results in")
                || between.contains("triggers")
                || between.contains("activates")
            {
                return Some("causes");
            }
            if between.contains("blocks")
                || between.contains("inhibits")
                || between.contains("prevents")
                || between.contains("suppresses")
            {
                return Some("inhibits");
            }
            if between.contains(" vs ")
                || between.contains(" versus ")
                || between.contains(" compared with ")
                || between.contains(" compared to ")
                || between.contains(" relative to ")
            {
                return Some("compared_with");
            }
            if between.contains("higher")
                || between.contains("greater")
                || between.contains("increased")
                || between.contains("improved")
                || between.contains("stronger")
                || between.contains("enhanced")
            {
                return Some("higher_than");
            }
            if between.contains("lower")
                || between.contains("smaller")
                || between.contains("decreased")
                || between.contains("reduced")
                || between.contains("weaker")
                || between.contains("suppressed")
            {
                return Some("lower_than");
            }
        }
    }
    None
}

fn has_intervening_panel_reference(
    text: &str,
    start: usize,
    end: usize,
    subject: &str,
    object: &str,
    panel_labels: &[String],
) -> bool {
    panel_labels
        .iter()
        .filter(|label| label.as_str() != subject && label.as_str() != object)
        .flat_map(|label| panel_reference_positions(text, label))
        .any(|span| span.start >= start && span.end <= end)
}

#[derive(Debug, Clone, Copy)]
struct TextSpan {
    start: usize,
    end: usize,
}

fn panel_reference_positions(text: &str, label: &str) -> Vec<TextSpan> {
    let mut positions = Vec::new();
    let lower = text.to_lowercase();
    let label_lower = label.to_lowercase();
    for needle in [
        format!("({label_lower})"),
        format!("panel {label_lower}"),
        format!("figure {label_lower}"),
        format!("fig. {label_lower}"),
    ] {
        positions.extend(find_case_insensitive_spans(&lower, &needle));
    }
    if label.chars().all(|ch| ch.is_ascii_uppercase()) {
        positions.extend(find_bare_panel_label_spans(text, label));
    }
    positions.sort_by_key(|span| span.start);
    positions.dedup_by_key(|span| (span.start, span.end));
    positions
}

fn find_case_insensitive_spans(lower_text: &str, lower_needle: &str) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    let mut offset = 0usize;
    while let Some(index) = lower_text[offset..].find(lower_needle) {
        let start = offset + index;
        let end = start + lower_needle.len();
        spans.push(TextSpan { start, end });
        offset = end;
    }
    spans
}

fn find_bare_panel_label_spans(text: &str, label: &str) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    let mut offset = 0usize;
    while let Some(index) = text[offset..].find(label) {
        let start = offset + index;
        let end = start + label.len();
        if is_panel_ref_boundary(text, start, end) {
            spans.push(TextSpan { start, end });
        }
        offset = end;
    }
    spans
}

fn is_panel_ref_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let before_ok = before.is_none_or(|ch| !ch.is_alphanumeric()) && before != Some('°');
    let after_ok = text[end..]
        .chars()
        .next()
        .is_none_or(|ch| !ch.is_alphanumeric());
    before_ok && after_ok
}

fn target_label_for_panel(
    panel_label: &str,
    panel_labels: &[String],
    target_labels: &[String],
) -> String {
    panel_labels
        .iter()
        .position(|candidate| candidate == panel_label)
        .and_then(|index| target_labels.get(index))
        .cloned()
        .unwrap_or_else(|| panel_label.to_string())
}

fn caption_panel_segments(text: &str) -> Vec<(Vec<String>, String)> {
    let text = text.split_once(':').map(|(_, tail)| tail).unwrap_or(text);
    let mut markers = Vec::new();
    for (index, ch) in text.char_indices() {
        if ch != '(' {
            continue;
        }
        let after_open = &text[index + ch.len_utf8()..];
        let Some(close_index) = after_open.find(')') else {
            continue;
        };
        if close_index > 5 {
            continue;
        }
        let labels = panel_marker_labels(&after_open[..close_index]);
        if labels.is_empty() {
            continue;
        }
        let content_start = index + ch.len_utf8() + close_index + ')'.len_utf8();
        markers.push((index, content_start, labels));
    }

    let mut segments = Vec::new();
    for (marker_index, (_, content_start, labels)) in markers.iter().enumerate() {
        let content_end = markers
            .get(marker_index + 1)
            .map(|(next_start, _, _)| *next_start)
            .unwrap_or(text.len());
        let description = clean_panel_description(&text[*content_start..content_end]);
        if !description.is_empty() {
            segments.push((labels.clone(), description));
        }
    }
    segments
}

fn clean_panel_description(description: &str) -> String {
    description
        .trim()
        .trim_matches(|ch: char| matches!(ch, ';' | ',' | '.'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn panel_marker_labels(marker: &str) -> Vec<String> {
    let marker = marker.trim().replace(['‐', '‑', '–', '—'], "-");
    if marker.contains(',') {
        let labels = marker
            .split(',')
            .map(str::trim)
            .filter(|label| single_panel_label(label).is_some())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if labels.len() > 1 {
            return labels;
        }
    }
    if let Some((first, last)) = marker.split_once('-') {
        let first = first.trim();
        let last = last.trim();
        if single_panel_label(first).is_some() && single_panel_label(last).is_some() {
            return expand_panel_labels(first, last);
        }
    }
    single_panel_label(&marker)
        .map(|_| vec![marker])
        .unwrap_or_default()
}

fn caption_targets_for_inferred_panels(
    caption_label: Option<&str>,
    object_label: &str,
    panel_labels: &[String],
) -> Vec<String> {
    let prefix = caption_label
        .and_then(|label| label.split_once(' ').map(|(prefix, _)| prefix))
        .unwrap_or("Figure");
    panel_labels
        .iter()
        .map(|panel| format!("{prefix} {object_label}{panel}"))
        .collect()
}

fn expand_panel_labels(first: &str, last: &str) -> Vec<String> {
    let Some(start) = single_panel_label(first) else {
        return vec![first.to_string(), last.to_string()];
    };
    let Some(end) = single_panel_label(last) else {
        return vec![first.to_string(), last.to_string()];
    };
    if start.is_ascii_uppercase() != end.is_ascii_uppercase()
        || start > end
        || end as u8 - start as u8 > 25
    {
        return vec![first.to_string(), last.to_string()];
    }
    (start as u8..=end as u8)
        .map(|letter| (letter as char).to_string())
        .collect()
}

fn single_panel_label(value: &str) -> Option<char> {
    let mut chars = value.chars();
    let first = chars.next()?;
    if chars.next().is_none() && first.is_ascii_alphabetic() {
        Some(first)
    } else {
        None
    }
}

#[derive(Debug)]
struct CaptionTokenSpan {
    value: String,
    start: usize,
    end: usize,
}

fn caption_token_spans(text: &str) -> Vec<CaptionTokenSpan> {
    let mut spans = Vec::new();
    let mut search_start = 0;
    for raw in text.split_whitespace() {
        let Some(offset) = text[search_start..].find(raw) else {
            continue;
        };
        let start = search_start + offset;
        let end = start + raw.len();
        search_start = end;
        let value = caption_token_value(raw);
        if !value.is_empty() {
            spans.push(CaptionTokenSpan { value, start, end });
        }
    }
    spans
}

fn caption_tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(caption_token_value)
        .filter(|token| !token.is_empty())
        .collect()
}

fn caption_token_value(token: &str) -> String {
    token
        .trim_matches(|ch: char| {
            matches!(
                ch,
                ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '"'
            )
        })
        .trim_end_matches('.')
        .to_string()
}

fn measurement_from_tokens(tokens: &[String], index: usize) -> Option<String> {
    let token = tokens.get(index)?;
    let mut parts = vec![token.clone()];
    if has_inline_unit(token) {
        return Some(parts.join(" "));
    }
    for next in tokens.iter().skip(index + 1).take(3) {
        if is_unit_token(next) {
            parts.push(next.clone());
        } else {
            break;
        }
    }
    if parts.len() > 1 {
        Some(parts.join(" "))
    } else {
        None
    }
}

fn named_measurement_from_tokens(
    tokens: &[String],
    index: usize,
    measurement: &str,
) -> Option<String> {
    let metric = metric_phrase_before(tokens, index)?;
    Some(format!("{metric} {measurement}"))
}

fn metric_phrase_before(tokens: &[String], index: usize) -> Option<String> {
    let mut end = index;
    while end > 0 && is_metric_connector(&tokens[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let start = end.saturating_sub(3);
    for candidate_start in start..end {
        let phrase = tokens[candidate_start..end].join(" ");
        if is_metric_phrase(&phrase) {
            return Some(phrase);
        }
    }
    None
}

fn is_metric_connector(token: &str) -> bool {
    matches!(
        token.to_lowercase().as_str(),
        "of" | "is"
            | "are"
            | "was"
            | "were"
            | "reached"
            | "reaches"
            | "reaching"
            | "achieved"
            | "achieves"
            | "showed"
            | "shows"
            | "gave"
            | "gives"
            | "="
            | "~"
            | "≈"
    )
}

fn is_metric_phrase(phrase: &str) -> bool {
    let lower = phrase.to_lowercase();
    matches!(
        lower.as_str(),
        "conversion"
            | "selectivity"
            | "yield"
            | "efficiency"
            | "faradaic efficiency"
            | "fe"
            | "current density"
            | "surface area"
            | "bet surface area"
            | "capacity"
            | "retention"
            | "overpotential"
            | "voltage"
            | "potential"
            | "temperature"
            | "pressure"
            | "ph"
            | "rate"
            | "tof"
            | "turnover frequency"
    )
}

fn condition_from_tokens(tokens: &[String], index: usize) -> String {
    let mut parts = Vec::new();
    for token in tokens.iter().skip(index).take(8) {
        if !parts.is_empty() && is_condition_starter(token) {
            break;
        }
        parts.push(token.clone());
    }
    parts.join(" ")
}

fn is_numeric_token(token: &str) -> bool {
    let mut chars = token.chars();
    let first = chars.next();
    let starts_numeric = match first {
        Some(ch) if ch.is_ascii_digit() => true,
        Some('+') | Some('-') | Some('−') => chars.next().is_some_and(|ch| ch.is_ascii_digit()),
        _ => false,
    };
    starts_numeric && token.chars().any(|ch| ch.is_ascii_digit())
}

fn has_inline_unit(token: &str) -> bool {
    token.contains('%')
        || token.chars().any(|ch| ch.is_alphabetic())
        || token.contains('°')
        || token.contains('µ')
        || token.contains('μ')
}

fn is_unit_token(token: &str) -> bool {
    if is_condition_starter(token) {
        return false;
    }
    token.chars().any(|ch| ch.is_alphabetic())
        && token.chars().all(|ch| {
            ch.is_alphanumeric()
                || matches!(
                    ch,
                    '%' | '°' | 'µ' | 'μ' | '-' | '−' | '+' | '/' | '^' | '·' | '.'
                )
        })
}

fn is_condition_starter(token: &str) -> bool {
    matches!(
        token.to_lowercase().as_str(),
        "under" | "at" | "in" | "with" | "using" | "during" | "after" | "before" | "for"
    )
}

fn push_unique(values: &mut Vec<String>, seen: &mut BTreeSet<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let key = value.to_lowercase();
    if seen.insert(key) {
        values.push(value.to_string());
    }
}

fn non_empty_json_array(values: &[String]) -> Option<String> {
    if values.is_empty() {
        None
    } else {
        serde_json::to_string(values).ok()
    }
}

pub fn split_text(text: &str, max_chars: usize, overlap: usize) -> Vec<String> {
    let normalized = text.trim();
    if normalized.is_empty() {
        return Vec::new();
    }
    if normalized.chars().count() <= max_chars {
        return vec![normalized.to_string()];
    }

    let chars: Vec<char> = normalized.chars().collect();
    let mut pieces = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let mut end = (start + max_chars).min(chars.len());
        if end < chars.len() {
            let lower_bound = start + max_chars / 2;
            for index in (lower_bound..end).rev() {
                if chars[index] == '\n' || chars[index] == '。' || chars[index] == '.' {
                    end = index + 1;
                    break;
                }
            }
        }
        let piece: String = chars[start..end].iter().collect();
        if !piece.trim().is_empty() {
            pieces.push(piece.trim().to_string());
        }
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(overlap).max(start + 1);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_long_text() {
        let chunks = split_text(&"A".repeat(1000), 300, 50);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 300));
    }

    #[test]
    fn carries_caption_metadata_into_chunks() {
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: std::path::PathBuf::new(),
            article_path: std::path::PathBuf::new(),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: Default::default(),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![Section {
                title: "Figure 2 Caption".to_string(),
                level: 2,
                content: "Figure 2: conversion reached 82% at 25 °C under 10 mA cm−2 for 2 h."
                    .to_string(),
            }],
        };

        let chunks = chunk_paper(&paper, 3200, 350);

        assert_eq!(chunks[0].section_kind, "figure_caption");
        assert_eq!(chunks[0].caption_label.as_deref(), Some("Figure 2"));
        assert_eq!(chunks[0].caption_object_type.as_deref(), Some("figure"));
        assert_eq!(chunks[0].caption_object_label.as_deref(), Some("2"));
        assert!(chunks[0].caption_panel_labels_json.is_none());
        assert!(chunks[0].caption_panel_details_json.is_none());
        assert_eq!(
            chunks[0].caption_target_labels_json.as_deref(),
            Some("[\"Figure 2\"]")
        );
        assert_eq!(
            chunks[0].caption_measurements_json.as_deref(),
            Some("[\"conversion 82%\",\"82%\",\"25 °C\",\"10 mA cm−2\",\"2 h\"]")
        );
        assert_eq!(
            chunks[0].caption_conditions_json.as_deref(),
            Some("[\"at 25 °C\",\"under 10 mA cm−2\",\"for 2 h\"]")
        );
        assert_eq!(
            chunks[0].caption_values_json.as_deref(),
            Some("[\"82%\",\"25\",\"10\",\"2\"]")
        );
    }

    #[test]
    fn extracts_named_caption_measurements_and_conditions() {
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: std::path::PathBuf::new(),
            article_path: std::path::PathBuf::new(),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: Default::default(),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![Section {
                title: "Table S1 Caption".to_string(),
                level: 2,
                content: "Table S1: Faradaic efficiency of 92% and current density of 10 mA cm−2 at pH 7.4 using 0.1 M KOH."
                    .to_string(),
            }],
        };

        let chunks = chunk_paper(&paper, 3200, 350);

        assert_eq!(
            chunks[0].caption_measurements_json.as_deref(),
            Some(
                "[\"Faradaic efficiency 92%\",\"92%\",\"current density 10 mA cm−2\",\"10 mA cm−2\",\"pH 7.4\",\"0.1 M KOH\"]"
            )
        );
        assert_eq!(
            chunks[0].caption_conditions_json.as_deref(),
            Some("[\"at pH 7.4\",\"using 0.1 M KOH\"]")
        );
    }

    #[test]
    fn infers_caption_panel_targets_from_caption_text() {
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: std::path::PathBuf::new(),
            article_path: std::path::PathBuf::new(),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: Default::default(),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![Section {
                title: "Figure 3 Caption".to_string(),
                level: 2,
                content: "Figure 3: (A) SEM image; (B) XRD pattern; (C) conversion reached 90%."
                    .to_string(),
            }],
        };

        let chunks = chunk_paper(&paper, 3200, 350);

        assert_eq!(
            chunks[0].caption_panel_labels_json.as_deref(),
            Some("[\"A\",\"B\",\"C\"]")
        );
        assert_eq!(
            chunks[0].caption_target_labels_json.as_deref(),
            Some("[\"Figure 3A\",\"Figure 3B\",\"Figure 3C\"]")
        );
        assert_eq!(
            chunks[0].caption_measurements_json.as_deref(),
            Some("[\"conversion 90%\",\"90%\"]")
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                chunks[0].caption_panel_details_json.as_deref().unwrap()
            )
            .unwrap(),
            serde_json::json!([
                {
                    "panel_label": "A",
                    "target_label": "Figure 3A",
                    "description": "SEM image",
                    "measurements": [],
                    "conditions": [],
                    "values": []
                },
                {
                    "panel_label": "B",
                    "target_label": "Figure 3B",
                    "description": "XRD pattern",
                    "measurements": [],
                    "conditions": [],
                    "values": []
                },
                {
                    "panel_label": "C",
                    "target_label": "Figure 3C",
                    "description": "conversion reached 90%",
                    "measurements": ["conversion 90%", "90%"],
                    "conditions": [],
                    "values": ["90%"]
                }
            ])
        );
    }

    #[test]
    fn extracts_caption_panel_comparison_relations() {
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: std::path::PathBuf::new(),
            article_path: std::path::PathBuf::new(),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: Default::default(),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![Section {
                title: "Figure 4 Caption".to_string(),
                level: 2,
                content:
                    "Figure 4: (A) catalyst sample; (B) control sample; (C) A shows higher conversion than B at 25 °C."
                        .to_string(),
            }],
        };

        let chunks = chunk_paper(&paper, 3200, 350);

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                chunks[0].caption_panel_details_json.as_deref().unwrap()
            )
            .unwrap(),
            serde_json::json!([
                {
                    "panel_label": "A",
                    "target_label": "Figure 4A",
                    "description": "catalyst sample",
                    "measurements": [],
                    "conditions": [],
                    "values": []
                },
                {
                    "panel_label": "B",
                    "target_label": "Figure 4B",
                    "description": "control sample",
                    "measurements": [],
                    "conditions": [],
                    "values": []
                },
                {
                    "panel_label": "C",
                    "target_label": "Figure 4C",
                    "description": "A shows higher conversion than B at 25 °C",
                    "measurements": ["25 °C"],
                    "conditions": ["at 25 °C"],
                    "values": ["25"],
                    "relations": [
                        {
                            "subject_panel_label": "A",
                            "subject_target_label": "Figure 4A",
                            "relation": "higher_than",
                            "object_panel_label": "B",
                            "object_target_label": "Figure 4B",
                            "evidence": "A shows higher conversion than B at 25 °C"
                        }
                    ]
                }
            ])
        );
    }

    #[test]
    fn extracts_caption_panel_causal_relations_without_skipping_through_intermediate_panels() {
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: std::path::PathBuf::new(),
            article_path: std::path::PathBuf::new(),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: Default::default(),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![Section {
                title: "Figure 5 Caption".to_string(),
                level: 2,
                content:
                    "Figure 5: (A) UV irradiation; (B) carrier separation; (C) recombination; (D) A induces B, while B suppresses C."
                        .to_string(),
            }],
        };

        let chunks = chunk_paper(&paper, 3200, 350);
        let details = serde_json::from_str::<serde_json::Value>(
            chunks[0].caption_panel_details_json.as_deref().unwrap(),
        )
        .unwrap();
        let panel_d = details
            .as_array()
            .unwrap()
            .iter()
            .find(|detail| detail["panel_label"] == "D")
            .unwrap();

        assert_eq!(
            panel_d["relations"],
            serde_json::json!([
                {
                    "subject_panel_label": "A",
                    "subject_target_label": "Figure 5A",
                    "relation": "causes",
                    "object_panel_label": "B",
                    "object_target_label": "Figure 5B",
                    "evidence": "A induces B, while B suppresses C"
                },
                {
                    "subject_panel_label": "B",
                    "subject_target_label": "Figure 5B",
                    "relation": "inhibits",
                    "object_panel_label": "C",
                    "object_target_label": "Figure 5C",
                    "evidence": "A induces B, while B suppresses C"
                }
            ])
        );
        assert_eq!(
            panel_d["relation_paths"],
            serde_json::json!([
                {
                    "start_panel_label": "A",
                    "start_target_label": "Figure 5A",
                    "via_panel_label": "B",
                    "via_target_label": "Figure 5B",
                    "end_panel_label": "C",
                    "end_target_label": "Figure 5C",
                    "relations": ["causes", "inhibits"],
                    "evidence": "A induces B, while B suppresses C"
                }
            ])
        );
    }

    #[test]
    fn extracts_caption_panel_cross_object_references() {
        let paper = Paper {
            author: "Alice".to_string(),
            paper_id: "paper-a".to_string(),
            paper_dir: std::path::PathBuf::new(),
            article_path: std::path::PathBuf::new(),
            fetch_result_path: None,
            source_hash: "hash".to_string(),
            metadata: Default::default(),
            fetch_result: serde_json::json!({}),
            raw_body: String::new(),
            clean_text: String::new(),
            sections: vec![Section {
                title: "Figure 6 Caption".to_string(),
                level: 2,
                content:
                    "Figure 6: (A) morphology follows Fig. 2B; (B) summary compared with Table S1; (C) self reference to Fig. 6A; (D) kinetic values are summarized in Table S2 and mechanism follows Fig. 4A; (E) calibration uses Figure 7; (F) enhancement is caused by Fig. 8A; (G) pathway is suppressed by Table S3."
                        .to_string(),
            }],
        };

        let chunks = chunk_paper(&paper, 3200, 350);
        let details = serde_json::from_str::<serde_json::Value>(
            chunks[0].caption_panel_details_json.as_deref().unwrap(),
        )
        .unwrap();
        let panel = |label: &str| {
            details
                .as_array()
                .unwrap()
                .iter()
                .find(|detail| detail["panel_label"] == label)
                .unwrap()
        };

        assert_eq!(
            panel("A")["cross_references"],
            serde_json::json!([
                {
                    "target_label": "Figure 2B",
                    "relation": "derived_from",
                    "evidence": "morphology follows Fig. 2B"
                }
            ])
        );
        assert_eq!(
            panel("B")["cross_references"],
            serde_json::json!([
                {
                    "target_label": "Table S1",
                    "relation": "compared_with",
                    "evidence": "summary compared with Table S1"
                }
            ])
        );
        assert!(panel("C").get("cross_references").is_none());
        assert_eq!(
            panel("D")["cross_references"],
            serde_json::json!([
                {
                    "target_label": "Table S2",
                    "relation": "summarized_in",
                    "evidence": "kinetic values are summarized in Table S2 and mechanism follows Fig. 4A"
                },
                {
                    "target_label": "Figure 4A",
                    "relation": "derived_from",
                    "evidence": "kinetic values are summarized in Table S2 and mechanism follows Fig. 4A"
                }
            ])
        );
        assert_eq!(
            panel("E")["cross_references"],
            serde_json::json!([
                {
                    "target_label": "Figure 7",
                    "relation": "references",
                    "evidence": "calibration uses Figure 7"
                }
            ])
        );
        assert_eq!(
            panel("F")["cross_references"],
            serde_json::json!([
                {
                    "target_label": "Figure 8A",
                    "relation": "caused_by",
                    "evidence": "enhancement is caused by Fig. 8A"
                }
            ])
        );
        assert_eq!(
            panel("G")["cross_references"],
            serde_json::json!([
                {
                    "target_label": "Table S3",
                    "relation": "inhibited_by",
                    "evidence": "pathway is suppressed by Table S3"
                }
            ])
        );
    }
}
