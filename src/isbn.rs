use crate::google_books::GoogleBooksFeed;
use crate::reference::{DataValue, Reference};
use anyhow::{Result, anyhow};
use grscraper::MetadataRequestBuilder;
use isbn::{Isbn10, Isbn13};
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::sync::Mutex;
use wikibase_rest_api::prelude::*;
use wikibase_rest_api::statements_patch::StatementsPatch;

lazy_static! {
    static ref RE_GOODREADS_ID: Regex = Regex::new(r"/(\d+)\.jpg$").unwrap();
    static ref RE_YEAR: Regex = Regex::new(r"^(\d{4})$").unwrap();
    static ref LANGUAGE_LABELS: HashMap<String, String> = {
        let json_string = include_str!("../static/languages.json");
        let data: HashMap<String, String> = serde_json::from_str(json_string).unwrap();
        data
    };
}

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
        v.try_into().map_err(|_| anyhow!("Wong length"))
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
