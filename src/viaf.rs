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
    headers.insert(header::ACCEPT, "application/json".parse().unwrap());
    headers.insert(
        header::USER_AGENT,
        "Wikidata Infernal Search Client/1.0".parse().unwrap(),
    );

    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()?;

    // URL encode the query
    let encoded_query = urlencoding::encode(query);

    // Construct the URL for searching VIAF
    // We specifically look for local.names in the query
    let url = format!(
        "https://viaf.org/viaf/search?query=local.names+=+{}&maximumRecords=10",
        encoded_query
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
        let cluster = &record["recordData"][nss(ns, "VIAFCluster")];
        let cluster = match cluster {
            Value::Object(_) => cluster,
            _ => continue,
        };
        let id = match cluster[nss(ns, "Document")]["about"].as_str() {
            Some(id) => id
                .trim_end_matches('/')
                .split('/')
                .next_back()
                .unwrap()
                .to_string(),
            None => continue,
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

        let label =
            match &cluster[nss(ns, "mainHeadings")][nss(ns, "data")][nss(ns, "text")].as_str() {
                Some(text) => text.to_string(),
                None => continue,
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
        ret.push(new_record);
    }

    Ok(ret)
}
