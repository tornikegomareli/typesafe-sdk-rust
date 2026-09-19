//! cargo run --example ticket
//! Needs TYPESAFE_API_KEY.

use serde_json::json;
use typesafe::{choice, noul, score, Questions, RequestOptions, SystemOneRequest, TypeSafeClient};

#[tokio::main]
async fn main() -> typesafe::Result<()> {
    let client = TypeSafeClient::new(Default::default())?;

    let models = client.models().list(RequestOptions::default()).await?;
    println!("models: {:?}", models.iter().map(|model| &model.name).collect::<Vec<_>>());

    let mut questions = Questions::new();
    questions.insert(
        "category".into(),
        choice(
            "What is this ticket about?",
            [
                ("billing", json!("money, charges, refunds")),
                ("technical", json!(null)),
                ("other", json!(null)),
            ],
        ),
    );
    questions.insert("urgent".into(), noul("Does the customer convey urgency?"));
    questions.insert("anger".into(), score("How angry is the customer?", ["calm", "annoyed", "furious"]));

    let answer = client
        .system_one(
            SystemOneRequest::new("I was charged twice. Please fix this ASAP.", questions),
            RequestOptions::default(),
        )
        .with_response()
        .await?;
    let result = &answer.data;
    println!("model: {}, request: {:?}", result.model, answer.request_id);
    println!("category: {:?}", result.choice("category").map(|c| (&c.choice, c.confidence)));
    println!("urgent: {:?}", result.noul("urgent").map(|n| n.noul));
    println!("anger: {:?}", result.score("anger").map(|s| (s.score, s.probabilities_by_score())));
    println!("usage: {:?}", result.usage);
    Ok(())
}
