//! Edition metadata from the Open Library ISBN API.

use crate::isbn::{HTTP_CLIENT, ISBN2wiki};
use crate::reference::{DataValue, Reference};
use anyhow::{Result, anyhow};
use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::LazyLock;
use wikibase_rest_api::prelude::*;

// Values are validated before being proposed as Wikidata statements.
// `[0-9]`, not `\d`, which also matches non-ASCII digits.
static RE_EDITION_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^OL[0-9]+M$").unwrap());
static RE_GOODREADS_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9]+$").unwrap());
// The year at the end of e.g. "2003", "May 2003", or "March 4, 2003".
static RE_YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|\s)([0-9]{4})$").unwrap());

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OpenLibraryEdition {
    key: String,
    title: Option<String>,
    subtitle: Option<String>,
    number_of_pages: Option<i64>,
    publish_date: Option<String>,
    languages: Vec<KeyRef>,
    identifiers: HashMap<String, Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct KeyRef {
    key: String,
}

impl OpenLibraryEdition {
    pub async fn load(isbn2wiki: &ISBN2wiki) -> Result<()> {
        let isbn = isbn2wiki.isbn_digits()?;
        let url = format!("https://openlibrary.org/isbn/{isbn}.json");
        let edition: Self = HTTP_CLIENT
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        edition.add_to(isbn2wiki)
    }

    fn add_to(&self, isbn2wiki: &ISBN2wiki) -> Result<()> {
        let edition_id = self
            .key
            .strip_prefix("/books/")
            .filter(|id| RE_EDITION_ID.is_match(id))
            .ok_or_else(|| anyhow!("No valid Open Library edition ID"))?;
        isbn2wiki.add_reference(
            "P648",
            DataValue::String(edition_id.to_string()),
            Reference::none(),
        );
        let reference = Reference::prop("P648", edition_id);
        let add = |property: &str, value: DataValue| {
            isbn2wiki.add_reference(property, value, reference.clone());
        };

        if let Some(language) = self.language() {
            let titles = [("P1476", &self.title), ("P1680", &self.subtitle)];
            for (property, text) in titles {
                if let Some(text) = text {
                    add(
                        property,
                        DataValue::Monolingual {
                            label: text.trim().to_string(),
                            language: language.to_string(),
                        },
                    );
                }
            }
        }

        if let Some(pages) = self.number_of_pages.filter(|pages| *pages > 0) {
            add("P1104", DataValue::Quantity(pages));
        }

        let year = self
            .publish_date
            .as_deref()
            .and_then(|date| Some(RE_YEAR.captures(date.trim())?.get(1)?.as_str().to_string()));
        if let Some(year) = year {
            add(
                "P577",
                DataValue::Date {
                    time: format!("+{year}-01-01T00:00:00Z"),
                    precision: TimePrecision::Year,
                },
            );
        }

        let goodreads_ids = self.identifiers.get("goodreads").into_iter().flatten();
        for id in goodreads_ids.filter(|id| RE_GOODREADS_ID.is_match(id)) {
            add("P2969", DataValue::String(id.to_owned()));
        }

        for item_id in self.identifiers.get("wikidata").into_iter().flatten() {
            isbn2wiki.add_existing_item(item_id);
        }

        Ok(())
    }

    /// The Wikidata language code for the edition's title, if it has exactly
    /// one language and that is a common one. Open Library uses MARC codes.
    fn language(&self) -> Option<&'static str> {
        let [language] = self.languages.as_slice() else {
            return None;
        };
        let code = match language.key.strip_prefix("/languages/")? {
            "ara" => "ar",
            "cat" => "ca",
            "chi" => "zh",
            "cze" => "cs",
            "dan" => "da",
            "dut" => "nl",
            "eng" => "en",
            "fin" => "fi",
            "fre" => "fr",
            "ger" => "de",
            "gre" => "el",
            "heb" => "he",
            "hun" => "hu",
            "ita" => "it",
            "jpn" => "ja",
            "kor" => "ko",
            "lat" => "la",
            "pol" => "pl",
            "por" => "pt",
            "rus" => "ru",
            "spa" => "es",
            "swe" => "sv",
            "tur" => "tr",
            _ => return None,
        };
        Some(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> OpenLibraryEdition {
        serde_json::from_str(include_str!("../test_files/open_library.json")).unwrap()
    }

    fn values_after_add(edition: &OpenLibraryEdition) -> HashMap<String, Vec<DataValue>> {
        let isbn2wiki = ISBN2wiki::default();
        edition.add_to(&isbn2wiki).unwrap();
        let values = isbn2wiki.values.lock().unwrap();
        values
            .iter()
            .map(|(p, dvs)| (p.to_owned(), dvs.keys().cloned().collect()))
            .collect()
    }

    #[test]
    fn test_add_to_maps_edition_fields() {
        let values = values_after_add(&fixture());
        let s = |v: &str| vec![DataValue::String(v.to_string())];
        assert_eq!(values["P648"], s("OL23140636M"));
        assert_eq!(values["P2969"], s("181309"));
        assert_eq!(values["P1104"], vec![DataValue::Quantity(671)]);
        assert_eq!(
            values["P1476"],
            vec![DataValue::Monolingual {
                label: "Crime and punishment".to_string(),
                language: "en".to_string(),
            }]
        );
        assert_eq!(
            values["P577"],
            vec![DataValue::Date {
                time: "+2003-01-01T00:00:00Z".to_string(),
                precision: TimePrecision::Year,
            }]
        );
    }

    #[test]
    fn test_add_to_records_valid_existing_items() {
        let edition = OpenLibraryEdition {
            identifiers: HashMap::from([(
                "wikidata".to_string(),
                vec!["Q22137041".to_string(), "Q1 } ?x".to_string()],
            )]),
            ..fixture()
        };
        let isbn2wiki = ISBN2wiki::default();
        edition.add_to(&isbn2wiki).unwrap();
        assert_eq!(isbn2wiki.existing_items(), ["Q22137041"]);
    }

    #[test]
    fn test_add_to_rejects_invalid_edition_id() {
        for key in [
            "",
            "/books/",
            "/books/OL1W",
            "/authors/OL1M",
            "/books/OL1M/x",
        ] {
            let edition = OpenLibraryEdition {
                key: key.to_string(),
                ..Default::default()
            };
            assert!(edition.add_to(&ISBN2wiki::default()).is_err(), "{key}");
        }
    }

    #[test]
    fn test_add_to_skips_invalid_or_ambiguous_values() {
        let edition = OpenLibraryEdition {
            title: Some("Bilingual".to_string()),
            number_of_pages: Some(0),
            publish_date: Some("c1994?".to_string()),
            languages: ["eng", "spa"]
                .map(|l| KeyRef {
                    key: format!("/languages/{l}"),
                })
                .into(),
            identifiers: HashMap::from([("goodreads".to_string(), vec!["12a".to_string()])]),
            ..fixture()
        };
        let values = values_after_add(&edition);
        assert_eq!(values.keys().collect::<Vec<_>>(), ["P648"]);
    }

    #[test]
    fn test_year_regex() {
        for (date, year) in [
            ("2003", "2003"),
            ("May 2003", "2003"),
            ("March 4, 2003", "2003"),
        ] {
            assert_eq!(&RE_YEAR.captures(date).unwrap()[1], year, "{date}");
        }
        for date in ["c1994", "2003-05-01", "12003", "٢٠٠٣"] {
            assert!(RE_YEAR.captures(date).is_none(), "{date}");
        }
    }
}
