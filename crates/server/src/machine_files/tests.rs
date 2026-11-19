use serde_json::{json, Value};

use super::*;

fn row(effort: &str, est_hours: &str, urls: &str) -> Value {
    json!({
        "severity": "high",
        "headline": "A problem",
        "effort": effort,
        "est_hours": est_hours,
        "confidence": "high",
        "citation_label": "OWASP A01",
        "citation_urls": urls,
        "status": "active"
    })
}

fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn a_row_gets_the_url_list_and_the_hour_numbers_beside_the_prose() {
    let findings = json!({ "findings": [row("medium", "1 to 2 days (about 8 to 16 hours)", "https://a.example/x\nhttps://b.example/y")] });
    let mut findings_json = serde_json::to_vec(&findings).expect("json");
    finish(&mut findings_json, &mut []).expect("finishes");
    let out: Value = serde_json::from_slice(&findings_json).expect("parses");
    let r = &out["findings"][0];
    assert_eq!(
        r["citation_url_list"],
        json!(["https://a.example/x", "https://b.example/y"])
    );
    assert_eq!(r["est_hours_min"], json!(8));
    assert_eq!(r["est_hours_max"], json!(16));
    // The prose is exactly what it was, and the new fields sit beside the ones they describe.
    assert_eq!(
        r["citation_urls"],
        "https://a.example/x\nhttps://b.example/y"
    );
    assert_eq!(r["est_hours"], "1 to 2 days (about 8 to 16 hours)");
    let k = keys(r);
    let at = |name: &str| k.iter().position(|x| x == name).expect(name);
    assert_eq!(at("citation_url_list"), at("citation_urls") + 1);
    assert_eq!(at("est_hours_min"), at("est_hours") + 1);
    assert_eq!(at("est_hours_max"), at("est_hours") + 2);
}

#[test]
fn every_effort_band_maps_to_its_hours_and_an_unestimated_row_has_null_bounds() {
    for (effort, low, high) in [("low", 2, 4), ("medium", 8, 16), ("high", 24, 40)] {
        let mut findings_json =
            serde_json::to_vec(&json!({ "findings": [row(effort, "x", "")] })).expect("json");
        finish(&mut findings_json, &mut []).expect("finishes");
        let out: Value = serde_json::from_slice(&findings_json).expect("parses");
        assert_eq!(out["findings"][0]["est_hours_min"], json!(low), "{effort}");
        assert_eq!(out["findings"][0]["est_hours_max"], json!(high), "{effort}");
        assert_eq!(out["findings"][0]["citation_url_list"], json!([]));
    }
    let mut findings_json =
        serde_json::to_vec(&json!({ "findings": [row("", "not yet estimated", "")] }))
            .expect("json");
    finish(&mut findings_json, &mut []).expect("finishes");
    let out: Value = serde_json::from_slice(&findings_json).expect("parses");
    assert_eq!(out["findings"][0]["est_hours_min"], Value::Null);
    assert_eq!(out["findings"][0]["est_hours_max"], Value::Null);
}

#[test]
fn finishing_twice_changes_nothing_the_second_time() {
    let mut findings_json = serde_json::to_vec(
        &json!({ "findings": [row("low", "2 to 4 hours", "https://a.example")] }),
    )
    .expect("json");
    finish(&mut findings_json, &mut []).expect("finishes");
    let once = findings_json.clone();
    finish(&mut findings_json, &mut []).expect("finishes");
    assert_eq!(once, findings_json);
}

#[test]
fn the_finding_datasets_get_the_fields_in_json_and_in_csv_alike() {
    let rows = json!([row(
        "high",
        "3 to 5 days (about 24 to 40 hours)",
        "https://a.example/x\nhttps://b.example/y"
    )]);
    let json_bytes = serde_json::to_vec_pretty(&rows).expect("json");
    let csv_bytes = {
        let mut w = csv::Writer::from_writer(Vec::new());
        let r = &rows[0];
        let object = r.as_object().expect("object");
        w.write_record(object.keys()).expect("header");
        w.write_record(object.values().map(csv_cell)).expect("row");
        w.into_inner().expect("csv")
    };
    let mut datasets = vec![
        ("json/all-findings.json".to_string(), json_bytes),
        ("csv/all-findings.csv".to_string(), csv_bytes),
    ];
    let mut findings_json = serde_json::to_vec(&json!({ "findings": [] })).expect("json");
    finish(&mut findings_json, &mut datasets).expect("finishes");

    let json_rows: Value = serde_json::from_slice(&datasets[0].1).expect("json");
    assert_eq!(json_rows[0]["est_hours_min"], json!(24));
    assert_eq!(json_rows[0]["est_hours_max"], json!(40));

    let mut reader = csv::Reader::from_reader(datasets[1].1.as_slice());
    let headers = reader.headers().expect("header").clone();
    let record = reader.records().next().expect("a row").expect("readable");
    let cell = |name: &str| {
        let at = headers.iter().position(|h| h == name).expect(name);
        record.get(at).expect("cell").to_string()
    };
    assert_eq!(cell("est_hours_min"), "24");
    assert_eq!(cell("est_hours_max"), "40");
    assert_eq!(
        cell("citation_url_list"),
        "[\"https://a.example/x\",\"https://b.example/y\"]"
    );
    assert_eq!(cell("est_hours"), "3 to 5 days (about 24 to 40 hours)");
    // The JSON and the CSV carry the same columns.
    let json_keys = keys(&json_rows[0]);
    assert_eq!(
        headers.iter().map(String::from).collect::<Vec<_>>(),
        json_keys
    );
}

#[test]
fn every_empty_csv_gets_its_columns_as_a_header_row() {
    for dataset in [
        "all-findings",
        "deviations",
        "conventions",
        "not-assessed",
        "dependencies",
        "false-positives",
        "coverage",
        "rules-applied",
    ] {
        // What the dataset writer produces for no rows: a header of one empty name.
        let mut datasets = vec![(format!("csv/{dataset}.csv"), b"\"\"\n".to_vec())];
        let mut findings_json = serde_json::to_vec(&json!({ "findings": [] })).expect("json");
        finish(&mut findings_json, &mut datasets).expect("finishes");
        let mut reader = csv::Reader::from_reader(datasets[0].1.as_slice());
        let headers: Vec<String> = reader
            .headers()
            .expect("a header")
            .iter()
            .map(String::from)
            .collect();
        assert!(headers.len() > 1, "{dataset}: {headers:?}");
        assert!(
            headers.iter().all(|h| !h.is_empty()),
            "{dataset}: {headers:?}"
        );
        assert_eq!(
            headers,
            columns_of(dataset)
                .expect("a known dataset")
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
        );
        assert!(reader.records().next().is_none(), "{dataset} has no rows");
    }
}

#[test]
fn a_csv_with_rows_keeps_its_own_header() {
    let mut datasets = vec![(
        "csv/coverage.csv".to_string(),
        b"rule_id,title\nA-1,A rule\n".to_vec(),
    )];
    let mut findings_json = serde_json::to_vec(&json!({ "findings": [] })).expect("json");
    finish(&mut findings_json, &mut datasets).expect("finishes");
    assert_eq!(datasets[0].1, b"rule_id,title\nA-1,A rule\n".to_vec());
}

#[test]
fn the_tickets_file_and_an_unknown_file_are_left_alone() {
    let tickets = b"Title,Description\n".to_vec();
    let mut datasets = vec![
        ("csv/tickets.csv".to_string(), tickets.clone()),
        ("csv/triage-log.csv".to_string(), b"\"\"\n".to_vec()),
    ];
    let mut findings_json = serde_json::to_vec(&json!({ "findings": [] })).expect("json");
    finish(&mut findings_json, &mut datasets).expect("finishes");
    assert_eq!(datasets[0].1, tickets);
    assert_eq!(datasets[1].1, b"\"\"\n".to_vec());
}
