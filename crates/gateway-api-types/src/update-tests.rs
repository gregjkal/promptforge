use super::*;

#[test]
fn an_answer_omits_absent_fields_and_round_trips() {
    let answer = UpdateAnswer {
        outcome: UpdateOutcome::Updating,
        version: Some("0.4.0".to_owned()),
        message: None,
    };
    let json = serde_json::to_string(&answer).expect("serialize");
    assert_eq!(json, r#"{"outcome":"updating","version":"0.4.0"}"#);
    let back: UpdateAnswer = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, answer);
}

#[test]
fn every_outcome_has_a_snake_case_name() {
    for (outcome, name) in [
        (UpdateOutcome::Updating, "\"updating\""),
        (UpdateOutcome::Current, "\"current\""),
        (UpdateOutcome::WorkshopOwned, "\"workshop_owned\""),
        (UpdateOutcome::Busy, "\"busy\""),
        (UpdateOutcome::Failed, "\"failed\""),
    ] {
        assert_eq!(serde_json::to_string(&outcome).expect("serialize"), name);
    }
}
