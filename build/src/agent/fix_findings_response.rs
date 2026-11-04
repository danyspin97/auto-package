//! A FixFindings turn's answer: what became of each finding in the Context.

use serde::Deserialize;

/// What a FixFindings turn says about one finding.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FindingAnswer {
    /// The finding's id, as the Context gives it.
    pub id: String,
    pub status: FindingStatus,
    #[serde(default)]
    pub reason: String,
}

/// The status a FixFindings turn gives a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    /// The edit fixes it.
    Fixed,
    /// It could not be fixed in the recipe.
    NotFixed,
    /// A person decides.
    NeedsMaintainer,
}

/// Adds the fields only a FixFindings turn answers: `finding_answers`
/// (required) and `changes_entry`.
pub(super) fn add_to_schema(schema: &mut serde_json::Value) {
    if let Some(required) = schema.get_mut("required").and_then(|r| r.as_array_mut()) {
        required.push("finding_answers".into());
    }
    schema["properties"]["finding_answers"] = serde_json::json!({
        "type": "array",
        "description": "FixFindings only: one entry for each finding in the Context.",
        "items": {
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "status": {"type": "string", "enum": ["fixed", "not_fixed", "needs_maintainer"]},
                "reason": {"type": "string"}
            },
            "required": ["id", "status", "reason"],
            "additionalProperties": false
        }
    });
    schema["properties"]["changes_entry"] = serde_json::json!({
        "type": "string",
        "description": "FixFindings only, optional: the bullets that replace the bullets of the request's own newest `.changes` entry, one bullet per line, every bullet kept that names a patch, says `Update to <version>` or cites a reference. Leave it empty when no finding is about the entry."
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_requires_the_answers_and_offers_the_entry() {
        let mut schema = serde_json::json!({"required": ["action"], "properties": {}});
        add_to_schema(&mut schema);
        assert_eq!(
            schema["required"],
            serde_json::json!(["action", "finding_answers"])
        );
        assert!(schema["properties"]["changes_entry"].is_object());
    }

    #[test]
    fn an_answer_parses_its_status_and_refuses_another() {
        let ok: FindingAnswer =
            serde_json::from_str(r#"{"id":"patch.header","status":"not_fixed","reason":"r"}"#)
                .unwrap();
        assert_eq!(ok.status, FindingStatus::NotFixed);
        assert!(serde_json::from_str::<FindingAnswer>(r#"{"id":"x","status":"done"}"#).is_err());
    }
}
