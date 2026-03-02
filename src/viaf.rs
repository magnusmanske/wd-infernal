use anyhow::{Context, Result};
use reqwest::header;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Default)]
pub struct RecordId {
    pub code: String,
    pub id: String,
    pub text: String,
}

impl RecordId {
    fn from_value(ns: usize, v: &Value) -> Option<Self> {
        let code = v[nss(ns, "sources")][nss(ns, "s")].as_str()?.to_string();
        let id = v[nss(ns, "sources")][nss(ns, "sid")].as_str()?.to_string();
        let text = v[nss(ns, "text")].as_str().unwrap_or_default().to_string();
        let id = id.split('|').nth(1).unwrap_or_else(|| &id).to_string();
        Some(Self { code, id, text })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub id: String,
    pub label: String,
    pub born: Option<String>,
    pub died: Option<String>,
    pub ids: Vec<RecordId>,
}

fn nss(nsid: usize, postfix: &str) -> String {
    format!("ns{nsid}:{postfix}")
}

pub async fn search_viaf_for_local_names(query: &str) -> Result<Vec<Record>> {
    let mut headers = header::HeaderMap::new();
    headers.insert(header::ACCEPT, "application/json".parse()?);
    headers.insert(
        header::USER_AGENT,
        "Wikidata Infernal Search Client/1.0".parse()?,
    );

    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()?;

    // URL encode the query
    let encoded_query = urlencoding::encode(query);

    // Construct the URL for searching VIAF
    // We specifically look for local.names in the query
    let url = format!(
        "https://viaf.org/viaf/search?query=local.names+=+{encoded_query}&maximumRecords=10"
    );

    // Make the HTTP request to VIAF
    let response = client
        .get(&url)
        .send()
        .await
        .context("Failed to send request to VIAF")?;

    // Check if the request was successful
    if !response.status().is_success() {
        anyhow::bail!("VIAF returned error status: {}", response.status());
    }

    let value: Value = response.json().await?;
    // println!("{:#?}", value);

    let records = &value["searchRetrieveResponse"]["records"]["record"];
    let records: Vec<Value> = match records {
        Value::Array(records) => records.to_owned(),
        Value::Object(_) => vec![records.to_owned()],
        _ => Vec::new(),
    };

    let mut ns = 1;
    let mut ret: Vec<Record> = Vec::new();
    for record in &records {
        ns += 1;
        match extract_local_name(ns, record) {
            Some(new_record) => ret.push(new_record),
            None => continue,
        }
    }

    Ok(ret)
}

fn extract_local_name(ns: usize, record: &Value) -> Option<Record> {
    let cluster = &record["recordData"][nss(ns, "VIAFCluster")];
    let cluster = match cluster {
        Value::Object(_) => cluster,
        _ => return None,
    };
    let id = match cluster[nss(ns, "Document")]["about"].as_str() {
        Some(id) => id.trim_end_matches('/').split('/').next_back()?.to_string(),
        None => return None,
    };
    let main_headings = &cluster[nss(ns, "mainHeadings")][nss(ns, "data")];
    let main_headings = match main_headings {
        Value::Object(_) => vec![main_headings.to_owned()],
        Value::Array(headings) => headings.to_owned(),
        _ => vec![],
    };
    let mut ids = vec![RecordId {
        code: "VIAF".to_string(),
        id: id.clone(),
        ..Default::default()
    }];
    ids.extend(
        main_headings
            .iter()
            .filter_map(|h| RecordId::from_value(ns, h)),
    );
    let label = match &cluster[nss(ns, "mainHeadings")][nss(ns, "data")][nss(ns, "text")].as_str() {
        Some(text) => text.to_string(),
        None => return None,
    };
    let new_record = Record {
        id,
        label,
        born: cluster[nss(ns, "birthDate")]
            .as_str()
            .map(|s| s.to_string()),
        died: cluster[nss(ns, "deathDate")]
            .as_str()
            .map(|s| s.to_string()),
        ids,
    };
    Some(new_record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── nss ───────────────────────────────────────────────────────────────────

    #[test]
    fn test_nss_formats_correctly() {
        assert_eq!(nss(1, "foo"), "ns1:foo");
        assert_eq!(nss(2, "VIAFCluster"), "ns2:VIAFCluster");
        assert_eq!(nss(10, "text"), "ns10:text");
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Build a minimal well-formed VIAF record JSON value using namespace 2.
    fn viaf_record(viaf_id: &str, label: &str, born: Option<&str>, died: Option<&str>) -> Value {
        let mut cluster = json!({
            "ns2:Document": { "about": format!("http://viaf.org/viaf/{viaf_id}/") },
            "ns2:mainHeadings": {
                "ns2:data": {
                    "ns2:text": label,
                    "ns2:sources": {
                        "ns2:s": "LC",
                        "ns2:sid": format!("LC|n{viaf_id}")
                    }
                }
            }
        });
        if let Some(b) = born {
            cluster["ns2:birthDate"] = json!(b);
        }
        if let Some(d) = died {
            cluster["ns2:deathDate"] = json!(d);
        }
        json!({ "recordData": { "ns2:VIAFCluster": cluster } })
    }

    // ── extract_local_name ────────────────────────────────────────────────────

    #[test]
    fn test_extract_local_name_full_record() {
        let record = viaf_record("12345", "Test Person", Some("1892"), Some("1973"));
        let result = extract_local_name(2, &record).expect("should parse a valid record");
        assert_eq!(result.id, "12345");
        assert_eq!(result.label, "Test Person");
        assert_eq!(result.born, Some("1892".to_string()));
        assert_eq!(result.died, Some("1973".to_string()));
    }

    #[test]
    fn test_extract_local_name_no_dates() {
        let record = viaf_record("99999", "No Dates", None, None);
        let result = extract_local_name(2, &record).expect("should parse without dates");
        assert!(result.born.is_none());
        assert!(result.died.is_none());
    }

    #[test]
    fn test_extract_local_name_trailing_slash_stripped() {
        // The "about" URL ends with "/" — the VIAF ID must be extracted without it
        let record = viaf_record("12345", "Test", None, None);
        let result = extract_local_name(2, &record).unwrap();
        assert_eq!(result.id, "12345");
    }

    #[test]
    fn test_extract_local_name_ids_include_viaf_cluster() {
        let record = viaf_record("77777", "Test", None, None);
        let result = extract_local_name(2, &record).unwrap();
        // First element is always the VIAF cluster ID itself
        assert_eq!(result.ids[0].code, "VIAF");
        assert_eq!(result.ids[0].id, "77777");
    }

    #[test]
    fn test_extract_local_name_ids_include_source() {
        // The source entry from main_headings is parsed and appended after the VIAF entry.
        // sid "LC|n77777" → after splitting on '|', the id part is "n77777"
        let record = viaf_record("77777", "Test", None, None);
        let result = extract_local_name(2, &record).unwrap();
        assert_eq!(result.ids.len(), 2);
        assert_eq!(result.ids[1].code, "LC");
        assert_eq!(result.ids[1].id, "n77777");
    }

    #[test]
    fn test_extract_local_name_missing_cluster_returns_none() {
        // The VIAFCluster key is absent
        let record = json!({ "recordData": {} });
        assert!(extract_local_name(2, &record).is_none());
    }

    #[test]
    fn test_extract_local_name_non_object_cluster_returns_none() {
        // VIAFCluster is a string, not an object
        let record = json!({ "recordData": { "ns2:VIAFCluster": "not-an-object" } });
        assert!(extract_local_name(2, &record).is_none());
    }

    #[test]
    fn test_extract_local_name_missing_document_about_returns_none() {
        // Document is present but the "about" field is missing
        let record = json!({
            "recordData": {
                "ns2:VIAFCluster": {
                    "ns2:Document": {},
                    "ns2:mainHeadings": {
                        "ns2:data": { "ns2:text": "Someone" }
                    }
                }
            }
        });
        assert!(extract_local_name(2, &record).is_none());
    }

    #[test]
    fn test_extract_local_name_missing_label_returns_none() {
        // The text field is absent from mainHeadings/data
        let record = json!({
            "recordData": {
                "ns2:VIAFCluster": {
                    "ns2:Document": { "about": "http://viaf.org/viaf/12345/" },
                    "ns2:mainHeadings": {
                        "ns2:data": {}
                    }
                }
            }
        });
        assert!(extract_local_name(2, &record).is_none());
    }

    // ── RecordId::from_value ──────────────────────────────────────────────────

    #[test]
    fn test_record_id_from_value_pipe_split() {
        // sid "LC|n79023149" → id should be the part after the first '|'
        let v = json!({
            "ns3:sources": { "ns3:s": "LC", "ns3:sid": "LC|n79023149" },
            "ns3:text": "Some name"
        });
        let rid = RecordId::from_value(3, &v).expect("should produce a RecordId");
        assert_eq!(rid.code, "LC");
        assert_eq!(rid.id, "n79023149");
        assert_eq!(rid.text, "Some name");
    }

    #[test]
    fn test_record_id_from_value_no_pipe_uses_full_sid() {
        // A sid without a '|' uses the entire value as the id
        let v = json!({
            "ns3:sources": { "ns3:s": "BNF", "ns3:sid": "abc123" },
            "ns3:text": ""
        });
        let rid = RecordId::from_value(3, &v).expect("should produce a RecordId");
        assert_eq!(rid.code, "BNF");
        assert_eq!(rid.id, "abc123");
    }

    #[test]
    fn test_record_id_from_value_missing_sources_returns_none() {
        // No "sources" key → cannot extract code or sid → must return None
        let v = json!({ "ns3:text": "Name" });
        assert!(RecordId::from_value(3, &v).is_none());
    }

    #[test]
    fn test_record_id_from_value_missing_text_defaults_to_empty_string() {
        // text uses unwrap_or_default, so a missing key must produce ""
        let v = json!({
            "ns3:sources": { "ns3:s": "WKP", "ns3:sid": "WKP|Q42" }
        });
        let rid = RecordId::from_value(3, &v).expect("should produce a RecordId");
        assert_eq!(rid.text, "");
        assert_eq!(rid.id, "Q42");
    }
}
