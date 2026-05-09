use serde_json::{Value, json};

pub fn parse_json_object(text: &str) -> Value {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return value;
    }

    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if end > start {
            if let Ok(value) = serde_json::from_str::<Value>(&text[start..=end]) {
                return value;
            }
        }
    }

    json!({ "raw_response": text })
}
