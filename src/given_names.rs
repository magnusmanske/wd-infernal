use crate::wikidata::Wikidata;
use anyhow::{Result, anyhow};
use axum::http::StatusCode;
use std::collections::HashMap;
use std::sync::OnceLock;
use wikimisc::entity_id::ItemId;

// Not in use now, might be useful for Person?

#[derive(Debug)]
pub struct GivenNames {
    male: HashMap<String, usize>,
    female: HashMap<String, usize>,
}

impl GivenNames {
    pub async fn get_static() -> Result<&'static GivenNames, StatusCode> {
        static ONCE: OnceLock<GivenNames> = OnceLock::new();
        if let Some(given_names) = ONCE.get() {
            return Ok(given_names);
        }
        let given_names = GivenNames::new()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(ONCE.get_or_init(|| given_names))
    }

    pub fn is_male(&self, name: &str) -> bool {
        self.male.contains_key(name)
    }

    pub fn is_female(&self, name: &str) -> bool {
        self.female.contains_key(name)
    }

    pub fn name2qid(&self, name: &str) -> Option<usize> {
        self.male.get(name).or(self.female.get(name)).cloned()
    }

    fn extract_names_for_gender(
        bindings: &[serde_json::Value],
        gender: ItemId,
    ) -> HashMap<String, usize> {
        let gender_uri = gender.uri();
        bindings
            .iter()
            .filter(|binding| binding["gender"]["value"] == gender_uri)
            .filter_map(|binding| {
                let item = ItemId::from_uri(binding["q"]["value"].as_str()?).ok()?;
                let label = binding["qLabel"]["value"].as_str()?;
                Some((label.to_lowercase(), usize::try_from(item.get()).ok()?))
            })
            .collect()
    }

    async fn new() -> Result<Self> {
        // Load all male and female given names, by their English and
        // multilingual labels.
        let sparql = r#"SELECT DISTINCT ?q (STR(?label) AS ?qLabel) ?gender {
          VALUES ?gender { wd:Q11879590 wd:Q12308941 }
          ?q wdt:P31 ?gender ; rdfs:label ?label .
          FILTER(LANG(?label) IN ("en", "mul"))
        }"#;
        let json = Wikidata::sparql_json(sparql)
            .await
            .map_err(|status| anyhow!("SPARQL query failed: {status}"))?;
        let bindings = json["results"]["bindings"]
            .as_array()
            .ok_or(anyhow!("results.bindings are not an array"))?;
        let male = Self::extract_names_for_gender(bindings, ItemId::new(12308941));
        let female = Self::extract_names_for_gender(bindings, ItemId::new(11879590));
        Ok(Self { male, female })
    }
}
