use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;

use crate::errors::{Error, Result};

/// Optional descriptions of the yes and no outcomes of a noul question.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct NoulCriteria {
    /// Description of the yes outcome.
    #[serde(rename = "true", skip_serializing_if = "Option::is_none")]
    pub yes: Option<Value>,
    /// Description of the no outcome.
    #[serde(rename = "false", skip_serializing_if = "Option::is_none")]
    pub no: Option<Value>,
}

/// A question identified by its `type` field.
///
/// Instructions and descriptions are text, a JSON object or array, or `null`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// A yes/no question with optional descriptions for either outcome.
    Noul {
        instructions: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// A question that selects between named alternatives. The labels keep their order, because
    /// the model sees them in this order.
    Choice {
        instructions: Value,
        /// Labels mapped to descriptions, or `null` for undescribed labels.
        criteria: IndexMap<String, Value>,
    },
    /// A question that assigns a score using an ordered rubric.
    Score {
        instructions: Value,
        /// At least two descriptions indexed by score from zero; `null` leaves a score undescribed.
        criteria: Vec<Value>,
    },
}

/// Questions keyed by the names used to identify their answers.
pub type Questions = IndexMap<String, Question>;

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// Create a yes/no question.
///
/// `instructions` is the question as text, a JSON object or array, or `Value::Null`.
pub fn noul(instructions: impl Into<Value>) -> Question {
    Question::Noul {
        instructions: instructions.into(),
        criteria: None,
    }
}

/// Create a yes/no question with descriptions of the yes and no outcomes.
pub fn noul_with_criteria(instructions: impl Into<Value>, yes: Option<Value>, no: Option<Value>) -> Question {
    Question::Noul {
        instructions: instructions.into(),
        criteria: Some(NoulCriteria { yes, no }),
    }
}

/// Create a score question using an ordered rubric.
///
/// `criteria` has at least two descriptions indexed by score from zero; entries may be `Value::Null`.
pub fn score<V: Into<Value>>(instructions: impl Into<Value>, criteria: impl IntoIterator<Item = V>) -> Question {
    Question::Score {
        instructions: instructions.into(),
        criteria: criteria.into_iter().map(Into::into).collect(),
    }
}

/// Create a question that selects between named alternatives.
///
/// `criteria` maps labels to descriptions, or to `Value::Null` for undescribed labels.
pub fn choice<K: Into<String>, V: Into<Value>>(instructions: impl Into<Value>, criteria: impl IntoIterator<Item = (K, V)>) -> Question {
    Question::Choice {
        instructions: instructions.into(),
        criteria: criteria
            .into_iter()
            .map(|(label, description)| (label.into(), description.into()))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Reject empty question sets and score questions without at least two criteria.
pub fn validate_questions(questions: &Questions) -> Result<()> {
    if questions.is_empty() {
        return Err(Error::TypeSafe("At least one question is required.".to_string()));
    }
    for (name, question) in questions {
        if let Question::Score { criteria, .. } = question {
            if criteria.len() < 2 {
                return Err(Error::TypeSafe(format!(
                    "Score question \"{name}\" has {} criteria; at least two scores are required.",
                    criteria.len()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_as_the_api_expects() {
        assert_eq!(
            json!(noul("Is it urgent?")),
            json!({"type": "noul", "instructions": "Is it urgent?"})
        );
        assert_eq!(json!(noul(Value::Null)), json!({"type": "noul", "instructions": null}));
        assert_eq!(
            json!(noul_with_criteria("Urgent?", Some(json!("yes, now")), None)),
            json!({"type": "noul", "instructions": "Urgent?", "criteria": {"true": "yes, now"}})
        );
        assert_eq!(
            json!(score("Rate it", ["bad", "good"])),
            json!({"type": "score", "instructions": "Rate it", "criteria": ["bad", "good"]})
        );
        assert_eq!(
            json!(choice("Pick", [("billing", json!("about money")), ("other", Value::Null)])),
            json!({"type": "choice", "instructions": "Pick", "criteria": {"billing": "about money", "other": null}})
        );
    }

    #[test]
    fn choice_keeps_the_order_of_the_labels() {
        let question = choice("Pick", [("zebra", Value::Null), ("apple", Value::Null), ("mango", Value::Null)]);
        let text = serde_json::to_string(&question).unwrap();
        assert!(text.find("zebra").unwrap() < text.find("apple").unwrap());
        assert!(text.find("apple").unwrap() < text.find("mango").unwrap());
    }

    #[test]
    fn validates() {
        assert_eq!(
            validate_questions(&Questions::new()).unwrap_err().to_string(),
            "At least one question is required."
        );
        let mut questions = Questions::new();
        questions.insert("rating".to_string(), score("Rate", ["only one"]));
        assert_eq!(
            validate_questions(&questions).unwrap_err().to_string(),
            "Score question \"rating\" has 1 criteria; at least two scores are required."
        );
        questions.insert("rating".to_string(), score("Rate", ["bad", "good"]));
        assert!(validate_questions(&questions).is_ok());
    }
}
