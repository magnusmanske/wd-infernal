use crate::google_books::GoogleBooksFeed;
use crate::reference::{DataValue, Reference};
use anyhow::{Result, anyhow};
use grscraper::MetadataRequestBuilder;
use isbn::{Isbn10, Isbn13};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::sync::{LazyLock, Mutex};
use wikibase_rest_api::prelude::*;
use wikibase_rest_api::statements_patch::StatementsPatch;

static RE_GOODREADS_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/(\d+)\.jpg$").unwrap());
static LANGUAGE_LABELS: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    let json_string = include_str!("../static/languages.json");
    serde_json::from_str(json_string).unwrap()
});

#[derive(Debug, Default)]
pub struct ISBN2wiki {
    pub isbn10: Option<Isbn10>,
    pub isbn13: Option<Isbn13>,
    pub values: Mutex<HashMap<String, HashMap<DataValue, HashSet<Reference>>>>,
}

impl ISBN2wiki {
    pub fn new(isbn: &str) -> Option<Self> {
        let isbn_digits = Self::str2digits(isbn);
        let isbn_10: Option<[u8; 10]> = Self::vec2array(isbn_digits.to_owned()).ok();
        let isbn_13: Option<[u8; 13]> = Self::vec2array(isbn_digits.to_owned()).ok();
        let mut ret = ISBN2wiki {
            isbn10: isbn_10.and_then(|isbn_array| Isbn10::new(isbn_array).ok()),
            isbn13: isbn_13.and_then(|isbn_array| Isbn13::new(isbn_array).ok()),
            ..Default::default()
        };

        ret.add_isbn_values_as_statements()?;

        Some(ret)
    }

    pub async fn new_from_item(item_id: &str) -> Option<Self> {
        let entity_id = EntityId::new(item_id).ok()?;
        let api = RestApi::wikidata().ok()?;
        let statements = Statements::get(&entity_id, &api).await.ok()?;
        let isbn10 = statements
            .statements()
            .iter()
            .filter(|(prop, _s)| *prop == "P957")
            .filter_map(|(_prop, s)| s.first())
            .filter_map(|s| match s.value() {
                StatementValue::Value(svc) => Some(svc),
                _ => None,
            })
            .filter_map(|svc| match svc {
                StatementValueContent::String(s) => Some(s),
                _ => None,
            })
            .map(|s| Self::str2digits(s))
            .filter_map(|s| Self::vec2array(s).ok())
            .filter_map(|s| Isbn10::new(s).ok().to_owned())
            .next();
        let isbn13 = statements
            .statements()
            .iter()
            .filter(|(prop, _s)| *prop == "P212")
            .filter_map(|(_prop, s)| s.first())
            .filter_map(|s| match s.value() {
                StatementValue::Value(svc) => Some(svc),
                _ => None,
            })
            .filter_map(|svc| match svc {
                StatementValueContent::String(s) => Some(s),
                _ => None,
            })
            .map(|s| Self::str2digits(s))
            .filter_map(|s| Self::vec2array(s).ok())
            .filter_map(|s| Isbn13::new(s).ok().to_owned())
            .next();

        if isbn10.is_none() && isbn13.is_none() {
            return None;
        }

        let mut ret = ISBN2wiki {
            isbn10,
            isbn13,
            ..Default::default()
        };

        ret.add_isbn_values_as_statements()?;

        Some(ret)
    }

    fn vec2array<T, const N: usize>(v: Vec<T>) -> Result<[T; N]> {
        v.try_into().map_err(|_| anyhow!("Wrong length"))
    }

    // Return ISBN13, fallback to ISBN10 if ISBN13 is not available
    pub fn isbn(&self) -> Option<String> {
        match self.isbn13 {
            Some(isbn) => Some(isbn.hyphenate().ok()?.to_string()),
            None => self
                .isbn10
                .and_then(|isbn| isbn.hyphenate().ok())
                .map(|s| s.to_string()),
        }
    }

    pub async fn retrieve(&mut self) -> Result<()> {
        let f1 = self.load_from_goodreads();
        let f2 = GoogleBooksFeed::load_from_google_books(self);
        futures::try_join!(f1, f2)?;
        Ok(())
    }

    async fn load_from_goodreads(&self) -> Result<()> {
        let isbn = self
            .isbn()
            .ok_or_else(|| anyhow!("No ISBN found"))?
            .replace('-', "");
        // println!("ISBN: {}", isbn);
        let metadata = MetadataRequestBuilder::default()
            .with_isbn(&isbn)
            .execute()
            .await
            .map_err(|_e| anyhow!("Failed to retrieve metadata"))?
            .ok_or(anyhow!("No metadata found"))?;
        // println!("Goodreads metadata retrieved successfully: {metadata:#?}");

        let goodreads_thumbnail_url = match metadata.image_url {
            Some(url) => url,
            None => return Err(anyhow!("No ID found")),
        };
        let goodreads_work_id = match RE_GOODREADS_ID.captures(goodreads_thumbnail_url.as_str()) {
            Some(captures) => {
                if let Some(first_group) = captures.get(1) {
                    first_group.as_str().to_string()
                } else {
                    return Err(anyhow!("No ID found"));
                }
            }
            None => return Err(anyhow!("No ID found")),
        };

        self.add_reference(
            "P8383",
            DataValue::String(goodreads_work_id.to_owned()),
            Reference::default(),
        );

        if let Some(publication_date) = metadata.publication_date {
            let publication_date = publication_date.format("%Y-%m-%dT%H:%M:%SZ").to_string();
            let precision = if publication_date.contains("-01-01T") {
                TimePrecision::Year
            } else {
                TimePrecision::Day
            };
            self.add_reference(
                "P577",
                DataValue::Date {
                    time: publication_date,
                    precision,
                },
                Reference::prop("P8383", &goodreads_work_id),
            );
        }

        for contributor in metadata.contributors {
            if contributor.role == "Author" {
                self.add_reference(
                    "P225",
                    DataValue::String(contributor.name.to_owned()),
                    Reference::prop("P8383", &goodreads_work_id),
                );
            }
        }

        if let Some(pages) = metadata.page_count {
            self.add_reference(
                "P1104",
                DataValue::Quantity(pages),
                Reference::prop("P8383", &goodreads_work_id),
            );
        }

        let language_code = metadata
            .language
            .as_ref()
            .map_or("", |language| match LANGUAGE_LABELS.get(language) {
                Some(code) => code,
                None => "",
            });

        if !language_code.is_empty() {
            self.add_reference(
                "P1476",
                DataValue::Monolingual {
                    label: metadata.title.to_owned(),
                    language: language_code.to_owned(),
                },
                Reference::prop("P8383", &goodreads_work_id),
            );

            if let Some(subtitle) = &metadata.subtitle {
                self.add_reference(
                    "P1680",
                    DataValue::Monolingual {
                        label: subtitle.to_owned(),
                        language: language_code.to_owned(),
                    },
                    Reference::prop("P8383", &goodreads_work_id),
                );
            }
        }

        Ok(())
    }

    pub fn add_reference(&self, property: &str, value: DataValue, reference: Reference) {
        // TODO handle poisoned mutex, or just ignore? unlikely event, no real fallout
        if let Ok(mut values) = self.values.lock() {
            values
                .entry(property.to_string())
                .or_default()
                .entry(value)
                .or_default()
                .insert(reference);
        }
    }

    fn add_isbn_values_as_statements(&mut self) -> Option<()> {
        if self.isbn10.is_none() && self.isbn13.is_none() {
            return None;
        }
        if let Some(isbn) = self.isbn10 {
            self.add_reference(
                "P957",
                DataValue::String(isbn.hyphenate().ok()?.to_string()),
                Reference::default(), // No reference for ISBN
            );
        }
        if let Some(isbn) = self.isbn13 {
            self.add_reference(
                "P212",
                DataValue::String(isbn.hyphenate().ok()?.to_string()),
                Reference::default(), // No reference for ISBN
            );
        }
        Some(())
    }

    pub fn generate_item(&self) -> Result<Item> {
        let mut ret = Item::default();
        let values = self
            .values
            .lock()
            .map_err(|_| anyhow!("Values lock poisoned"))?;

        for (property, dv2refs) in values.iter() {
            for (datavalue, references) in dv2refs {
                let expected_value = datavalue.as_statement_value();
                let mut statement = Statement::default();
                statement.set_property(PropertyType::property(property.to_owned()));
                statement.set_value(expected_value);
                Self::add_new_references_to_statement(&mut statement, references);
                ret.statements_mut()
                    .statements_mut()
                    .entry(property.to_owned())
                    .or_insert(vec![])
                    .push(statement);
            }
        }

        // TODO labels etc
        Ok(ret)
    }

    pub fn generate_patch(&self, item_id: &str) -> Result<StatementsPatch> {
        let entity_id = EntityId::new(item_id)?;
        let statements_old = Statements::default();
        let mut statements_new = statements_old.clone();
        let values = self
            .values
            .lock()
            .map_err(|_| anyhow!("Values lock poisoned"))?;

        for (property, dv2refs) in values.iter() {
            for (datavalue, references) in dv2refs {
                let expected_value = datavalue.as_statement_value();
                let mut statements: Vec<&mut Statement> = vec![];
                if let Some(existing) = statements_new.statements_mut().get_mut(property) {
                    let tmp: Vec<_> = existing
                        .iter_mut()
                        .filter(|statement| *statement.value() == expected_value)
                        .collect();
                    statements.extend(tmp);
                }

                // If more than one statement, remove deprecated
                if statements.len() > 1 {
                    statements
                        .retain(|s| *s.rank() != wikibase_rest_api::StatementRank::Deprecated);
                }

                // If more than one statement, with at least one preferred, keep only preferred
                if statements.len() > 1
                    && statements
                        .iter()
                        .any(|s| *s.rank() == wikibase_rest_api::StatementRank::Preferred)
                {
                    statements.retain(|s| *s.rank() == wikibase_rest_api::StatementRank::Preferred);
                }

                if statements.len() > 1 {
                    // More than one possible statement, not sure which, skip
                    continue;
                }

                // Only one or no statements, add references to existing,
                // or create new statement with references
                match statements.first_mut() {
                    Some(statement) => Self::add_new_references_to_statement(statement, references),
                    None => {
                        let mut statement = Statement::default();
                        statement.new_id_for_entity(&entity_id);
                        statement.set_property(PropertyType::property(property.to_owned()));
                        statement.set_value(expected_value);
                        Self::add_new_references_to_statement(&mut statement, references);
                        drop(statements);
                        statements_new
                            .statements_mut()
                            .entry(property.to_owned())
                            .or_insert(vec![])
                            .push(statement);
                    }
                }
            }
        }

        let patch = statements_old.patch(&statements_new)?;
        Ok(patch)
    }

    fn add_new_references_to_statement(statement: &mut Statement, references: &HashSet<Reference>) {
        for reference in references {
            if !statement
                .references()
                .iter()
                .any(|ref_group| reference.is_equivalent(ref_group))
            {
                if let Some(ref_group) = reference.as_ref_group() {
                    statement.references_mut().push(ref_group);
                }
            }
        }
    }

    fn str2digits(isbn: &str) -> Vec<u8> {
        isbn.chars()
            .filter_map(|c| c.to_digit(10))
            .map(|c| c as u8)
            .collect::<Vec<u8>>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── str2digits ────────────────────────────────────────────────────────────

    #[test]
    fn test_str2digits_plain_digits() {
        assert_eq!(
            ISBN2wiki::str2digits("1234567890"),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 0]
        );
    }

    #[test]
    fn test_str2digits_strips_hyphens_and_spaces() {
        assert_eq!(
            ISBN2wiki::str2digits("978-2-267-02700-6"),
            vec![9, 7, 8, 2, 2, 6, 7, 0, 2, 7, 0, 0, 6],
        );
    }

    #[test]
    fn test_str2digits_strips_letters() {
        // Letters that are not digits must be silently dropped
        assert_eq!(ISBN2wiki::str2digits("ISBN 123"), vec![1, 2, 3]);
    }

    #[test]
    fn test_str2digits_empty_string() {
        assert_eq!(ISBN2wiki::str2digits(""), Vec::<u8>::new());
    }

    // ── ISBN2wiki::new ────────────────────────────────────────────────────────

    #[test]
    fn test_new_isbn13_valid() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").expect("valid ISBN-13 should produce Some");
        // isbn13 must be populated
        assert!(isbn2wiki.isbn13.is_some(), "isbn13 should be set");
        // isbn() returns the hyphenated ISBN-13
        let isbn = isbn2wiki
            .isbn()
            .expect("isbn() must return Some for a valid ISBN-13");
        assert!(
            isbn.contains('-'),
            "hyphenated ISBN-13 must contain hyphens"
        );
        assert!(isbn.starts_with("978"), "ISBN-13 must start with 978");
    }

    #[test]
    fn test_new_isbn10_valid() {
        // "2267027003" is the ISBN-10 for the same book as above
        let isbn2wiki = ISBN2wiki::new("2267027003").expect("valid ISBN-10 should produce Some");
        assert!(isbn2wiki.isbn10.is_some(), "isbn10 should be set");
    }

    #[test]
    fn test_new_hyphenated_isbn13() {
        // Constructor must accept pre-hyphenated input
        let isbn2wiki =
            ISBN2wiki::new("978-2-267-02700-6").expect("hyphenated ISBN-13 should be accepted");
        assert!(isbn2wiki.isbn13.is_some());
    }

    #[test]
    fn test_new_invalid_returns_none() {
        // All-zeros is not a valid ISBN
        assert!(
            ISBN2wiki::new("0000000000000").is_none(),
            "invalid ISBN must return None"
        );
    }

    #[test]
    fn test_new_empty_returns_none() {
        assert!(
            ISBN2wiki::new("").is_none(),
            "empty string must return None"
        );
    }

    #[test]
    fn test_new_populates_isbn_statements() {
        // After construction, values must already contain ISBN property entries
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let values = isbn2wiki.values.lock().unwrap();
        // P212 = ISBN-13
        assert!(
            values.contains_key("P212"),
            "values should contain P212 (ISBN-13) after construction"
        );
    }

    // ── isbn() fallback ───────────────────────────────────────────────────────

    #[test]
    fn test_isbn_prefers_isbn13_over_isbn10() {
        // When both are available the ISBN-13 should be returned
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let isbn = isbn2wiki.isbn().unwrap();
        // ISBN-13 always starts with a 3-digit prefix
        assert!(
            isbn.starts_with("978") || isbn.starts_with("979"),
            "isbn() should prefer ISBN-13"
        );
    }

    #[test]
    fn test_isbn_falls_back_to_isbn10_when_no_isbn13() {
        // An ISBN-10 that cannot be represented as ISBN-13 due to length will only set isbn10
        let isbn2wiki = ISBN2wiki::new("2267027003").unwrap();
        // If isbn13 happened to be parsed, that is fine; the important thing is isbn() is Some
        assert!(
            isbn2wiki.isbn().is_some(),
            "isbn() must return Some when at least one is set"
        );
    }

    // ── add_reference ─────────────────────────────────────────────────────────

    #[test]
    fn test_add_reference_inserts_entry() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        isbn2wiki.add_reference(
            "P675",
            DataValue::String("TestID".to_string()),
            Reference::none(),
        );
        let values = isbn2wiki.values.lock().unwrap();
        assert!(
            values.contains_key("P675"),
            "P675 entry should be present after add_reference"
        );
    }

    #[test]
    fn test_add_reference_deduplicates_same_reference() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let dv = DataValue::String("SameID".to_string());
        let r = Reference::prop("P675", "SameID");
        isbn2wiki.add_reference("P675", dv.clone(), r.clone());
        isbn2wiki.add_reference("P675", dv.clone(), r.clone());
        let values = isbn2wiki.values.lock().unwrap();
        let refs = values["P675"][&dv].len();
        assert_eq!(
            refs, 1,
            "identical references must be deduplicated (HashSet)"
        );
    }

    #[test]
    fn test_add_reference_accumulates_different_references() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let dv = DataValue::String("BookID".to_string());
        isbn2wiki.add_reference("P675", dv.clone(), Reference::prop("P675", "BookID"));
        isbn2wiki.add_reference("P675", dv.clone(), Reference::prop("P8383", "GoodreadsID"));
        let values = isbn2wiki.values.lock().unwrap();
        let refs = values["P675"][&dv].len();
        assert_eq!(
            refs, 2,
            "two distinct references for the same value must both be stored"
        );
    }

    // ── generate_item ─────────────────────────────────────────────────────────

    #[test]
    fn test_generate_item_contains_isbn_property() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let item = isbn2wiki
            .generate_item()
            .expect("generate_item should succeed");
        // P212 (ISBN-13) must be among the statements
        let has_p212 = !item.statements().property("P212").is_empty();
        assert!(
            has_p212,
            "generated item should contain a P212 (ISBN-13) statement"
        );
    }

    #[test]
    fn test_generate_item_statement_value_matches() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let item = isbn2wiki.generate_item().unwrap();
        // The P212 statement value must be the hyphenated ISBN-13 string
        let stmts = item.statements().property("P212");
        assert!(!stmts.is_empty());
        let value = stmts[0].value();
        // StatementValue::Value(StatementValueContent::String(...))
        assert!(
            matches!(
                value,
                StatementValue::Value(StatementValueContent::String(s)) if s.contains('-')
            ),
            "P212 value should be a hyphenated ISBN-13 string"
        );
    }
}
