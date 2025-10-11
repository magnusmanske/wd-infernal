use crate::wikidata::Wikidata;
use anyhow::{Result, anyhow};
use mediawiki::Api;
use std::collections::HashMap;
use tokio::sync::OnceCell;

// Not in use now, might be useful for Person?

#[derive(Debug)]
pub struct GivenNames {
    male: HashMap<String, usize>,
    female: HashMap<String, usize>,
}

impl GivenNames {
    #![allow(clippy::missing_panics_doc)]
    pub async fn get_static() -> &'static GivenNames {
        static ONCE: OnceCell<GivenNames> = OnceCell::const_new();
        let api = Wikidata::get_wikidata_api()
            .await
            .expect("Wikidata API not available");
        ONCE.get_or_init(|| async {
            GivenNames::new(api)
                .await
                .expect("Failed to fetch given names")
        })
        .await
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

    async fn new(api: Api) -> Result<Self> {
        // Load all male and female given names from SPARQL
        let sparql = "SELECT ?q ?qLabel ?gender {
        	VALUES ?gender { wd:Q11879590 wd:Q12308941 } .
         	?q wdt:P31 ?gender .
          	SERVICE wikibase:label { bd:serviceParam wikibase:language \"[AUTO_LANGUAGE],en,mul\" }
           }";
        let json = api.sparql_query(sparql).await?;
        let bindings = json["results"]["bindings"]
            .as_array()
            .ok_or(anyhow!("results.bindings are not an array"))?;
        let male = bindings
            .iter()
            .filter(|binding| {
                binding["gender"]["value"] == "http://www.wikidata.org/entity/Q12308941"
            })
            .map(|binding| {
                (
                    binding["q"]["value"].as_str(),
                    binding["qLabel"]["value"].as_str(),
                )
            })
            .filter_map(|(name_opt, q_opt)| match (name_opt, q_opt) {
                (Some(name), Some(q)) => {
                    Some((name.to_lowercase(), q.rsplit("/").last()?.parse().ok()?))
                }
                _ => None,
            })
            .collect();
        let female = bindings
            .iter()
            .filter(|binding| {
                binding["gender"]["value"] == "http://www.wikidata.org/entity/Q11879590"
            })
            .map(|binding| {
                (
                    binding["q"]["value"].as_str(),
                    binding["qLabel"]["value"].as_str(),
                )
            })
            .filter_map(|(name_opt, q_opt)| match (name_opt, q_opt) {
                (Some(name), Some(q)) => {
                    Some((name.to_lowercase(), q.rsplit("/").last()?.parse().ok()?))
                }
                _ => None,
            })
            .collect();
        Ok(Self { male, female })
    }
}
