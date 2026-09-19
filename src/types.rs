use std::collections::HashMap;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::questions::Questions;
use crate::retry::RetryOverrides;

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

/// A yes/no answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct NoulResponse {
    /// Probability of a yes answer, from zero to one.
    pub noul: f64,
}

/// A selected label and its probabilities.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChoiceResponse {
    /// The selected label.
    pub choice: String,
    /// Reported confidence in the selected label.
    pub confidence: f64,
    /// Probabilities keyed by label.
    pub probabilities: IndexMap<String, f64>,
}

/// An expected score with its rubric and probabilities.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ScoreResponse {
    /// Expected score, which may fall between integer rubric levels.
    pub score: f64,
    /// Reported confidence in the score.
    pub confidence: f64,
    /// Rubric descriptions keyed by score, as in `"0"`, `"1"`.
    #[serde(default)]
    pub legend: IndexMap<String, Value>,
    /// Probabilities keyed by score, as in `"0"`, `"1"`.
    pub probabilities: IndexMap<String, f64>,
}

impl ScoreResponse {
    /// The probabilities as a list indexed by score.
    pub fn probabilities_by_score(&self) -> Vec<f64> {
        (0..self.probabilities.len())
            .map(|score| self.probabilities.get(&score.to_string()).copied().unwrap_or(0.0))
            .collect()
    }
}

/// The answer to one question, identified by its `type` field.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul(NoulResponse),
    Choice(ChoiceResponse),
    Score(ScoreResponse),
}

/// Token usage for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Usage {
    /// Number of input tokens used.
    pub input_tokens: u64,
    /// Number of output tokens used.
    pub output_tokens: u64,
}

/// Answers keyed by question name, with model and usage metadata.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SystemOneResult {
    /// The model used to answer the request.
    pub model: String,
    /// Answers keyed by the names of the questions.
    pub answers: HashMap<String, Answer>,
    /// Token usage for the request.
    pub usage: Usage,
}

impl SystemOneResult {
    /// The answer to the noul question `name`. `None` when the name or the type does not match.
    pub fn noul(&self, name: &str) -> Option<&NoulResponse> {
        match self.answers.get(name)? {
            Answer::Noul(answer) => Some(answer),
            _ => None,
        }
    }

    /// The answer to the choice question `name`. `None` when the name or the type does not match.
    pub fn choice(&self, name: &str) -> Option<&ChoiceResponse> {
        match self.answers.get(name)? {
            Answer::Choice(answer) => Some(answer),
            _ => None,
        }
    }

    /// The answer to the score question `name`. `None` when the name or the type does not match.
    pub fn score(&self, name: &str) -> Option<&ScoreResponse> {
        match self.answers.get(name)? {
            Answer::Score(answer) => Some(answer),
            _ => None,
        }
    }
}

/// Metadata for an available model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelCard {
    pub name: String,
    pub description: String,
    pub release_date: String,
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// State and named questions for `system_one`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SystemOneRequest {
    /// Text, a JSON object or array, or `null` to evaluate.
    pub state: Value,
    /// Nonempty questions keyed by the names used to identify their answers.
    pub questions: Questions,
    /// Model override; `None` inherits the default model of the client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Additional properties. They are forwarded in the request body, including `null` values.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl SystemOneRequest {
    pub fn new(state: impl Into<Value>, questions: Questions) -> Self {
        SystemOneRequest {
            state: state.into(),
            questions,
            model: None,
            extra: Map::new(),
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

/// Per-call options that override client settings.
#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
    /// Cancellation signal for the request and pending retries.
    pub signal: Option<CancellationToken>,
    /// Timeout per attempt in milliseconds; there is no total retry budget.
    pub timeout_ms: Option<u64>,
    /// Retry overrides for this call; omitted fields inherit client settings.
    pub retry: Option<RetryOverrides>,
    /// Additional headers, merged over the default headers. A `None` value removes the header.
    pub headers: Vec<(String, Option<String>)>,
}
