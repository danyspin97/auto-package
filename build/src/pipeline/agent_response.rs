//! The response an agent turn reports, as its event and as `response.json`:
//! the decision, the evidence, the changelog entry and, for a refresh turn, the
//! answer for each patch. The raw answer keeps the same fields in full.

use crate::agent::TurnOutcome;
use ap_core::event::{AgentResponseData, PatchAnswerData};

pub(super) fn response_data(outcome: &TurnOutcome) -> AgentResponseData {
    AgentResponseData {
        action: format!("{:?}", outcome.action),
        reasoning: outcome.reasoning.clone(),
        evidence: outcome.evidence.clone(),
        confidence: Some(format!("{:?}", outcome.confidence)),
        changelog: outcome.changelog.clone(),
        patches: outcome
            .patch_answers
            .iter()
            .map(|a| PatchAnswerData {
                name: a.name.clone(),
                status: format!("{:?}", a.status),
                reason: a.reason.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Action, PatchAnswer, PatchAnswerStatus};

    #[test]
    fn the_response_carries_the_changelog_and_the_patch_answers() {
        let mut outcome = crate::pipeline::patch_loop::pipeline_wheel_outcome("x");
        outcome.action = Action::Edited;
        outcome.changelog = "Rebase a.patch".into();
        outcome.patch_answers = vec![PatchAnswer {
            name: "a.patch".into(),
            status: PatchAnswerStatus::Rebased,
            reason: "ported".into(),
        }];
        let json = serde_json::to_value(response_data(&outcome)).unwrap();
        assert_eq!(json["changelog"], "Rebase a.patch", "{json}");
        assert_eq!(json["patches"][0]["name"], "a.patch", "{json}");
        assert_eq!(json["patches"][0]["status"], "Rebased", "{json}");
        assert_eq!(json["patches"][0]["reason"], "ported", "{json}");
    }

    #[test]
    fn an_older_response_without_the_fields_still_reads() {
        let old = r#"{"action":"NoChange","reasoning":"r","evidence":[],"confidence":"High"}"#;
        let data: AgentResponseData = serde_json::from_str(old).unwrap();
        assert!(data.patches.is_empty());
        assert!(data.changelog.is_empty());
    }
}
