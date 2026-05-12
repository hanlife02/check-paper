pub fn query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut current_is_cjk = false;

    for ch in query.chars() {
        let is_cjk = ('\u{4e00}'..='\u{9fff}').contains(&ch);
        let is_word = ch.is_alphanumeric() || matches!(ch, '-' | '_' | '/' | '.');
        if is_word {
            if !current.is_empty() && current_is_cjk != is_cjk {
                push_term(&mut terms, &mut current);
            }
            current_is_cjk = is_cjk;
            current.push(ch);
        } else {
            push_term(&mut terms, &mut current);
        }
    }
    push_term(&mut terms, &mut current);

    if terms.is_empty() && !query.trim().is_empty() {
        terms.push(query.trim().to_string());
    }
    enrich_query_terms(&mut terms, query);
    terms
}

fn enrich_query_terms(terms: &mut Vec<String>, query: &str) {
    for token in query.split_whitespace() {
        let normalized = token.trim_matches(|ch: char| {
            !ch.is_ascii_alphanumeric() && !matches!(ch, '.' | '/' | '-' | '_')
        });
        if normalized.starts_with("10.") && normalized.contains('/') {
            push_unique_term(terms, normalized);
        }
        if normalized.chars().any(|ch| ch.is_ascii_digit()) {
            push_unique_term(terms, normalized);
        }
    }

    let quoted = query
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::trim)
        .filter(|item| item.chars().count() > 2)
        .collect::<Vec<_>>();
    for phrase in quoted {
        push_unique_term(terms, phrase);
    }

    let english_phrase = query
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-' && ch != ' ')
        .map(str::trim)
        .filter(|item| item.split_whitespace().count() >= 2)
        .collect::<Vec<_>>();
    for phrase in english_phrase {
        if phrase.chars().count() <= 80 {
            push_unique_term(terms, phrase);
        }
    }
}

fn push_term(terms: &mut Vec<String>, current: &mut String) {
    let term = current.trim();
    if !term.is_empty()
        && (term.chars().count() > 1 || term.chars().all(|ch| ch.is_ascii_alphanumeric()))
    {
        push_unique_term(terms, term);
    }
    current.clear();
}

fn push_unique_term(terms: &mut Vec<String>, term: &str) {
    let term = term.trim();
    if !term.is_empty()
        && !terms
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(term))
    {
        terms.push(term.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::query_terms;

    #[test]
    fn query_terms_rewrite_keeps_doi_numbers_and_phrases() {
        let terms = query_terms("Compare \"MOF catalyst\" 82% DOI 10.1000/paper-a");
        assert!(terms.iter().any(|term| term == "10.1000/paper-a"));
        assert!(terms.iter().any(|term| term.contains("82")));
        assert!(terms.iter().any(|term| term == "MOF catalyst"));
    }
}
