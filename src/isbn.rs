use anyhow::{anyhow, Result};
use futures::join;
use grscraper::MetadataRequestBuilder;
use isbn::{Isbn10, Isbn13};
use lazy_static::lazy_static;
use regex::Regex;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::sync::Mutex;
use wikibase_rest_api::prelude::*;
use wikibase_rest_api::property_value::PropertyValue;
use wikibase_rest_api::statements_patch::StatementsPatch;

lazy_static! {
    static ref RE_GOODREADS_ID: Regex = Regex::new(r"/(\d+)\.jpg$").unwrap();
    static ref RE_GOOGLE_BOOKS_ID: Regex = Regex::new(r"^([a-zA-Z0-9]+)$").unwrap();
    static ref RE_ISBN_10: Regex = Regex::new(r"^ISBN:(\d{9}[0-9X])$").unwrap();
    static ref RE_ISBN_13: Regex = Regex::new(r"^ISBN:(\d{12}[0-9X])$").unwrap();
    static ref RE_PAGES: Regex = Regex::new(r"^(\d+) pages$").unwrap();
    static ref RE_YEAR: Regex = Regex::new(r"^(\d{4})$").unwrap();
    static ref LANGUAGE_LABELS: HashMap<String, String> = {
        let json_string = include_str!("../static/languages.json");
        let data: HashMap<String, String> = serde_json::from_str(json_string).unwrap();
        data
    };
}

#[derive(Debug, Deserialize, PartialEq)]
struct GoogleBooksEntry {
    id: Vec<String>,
    title: String,
    #[serde(default)]
    identifier: Vec<String>,
    #[serde(default)]
    dctitle: Vec<String>,
    #[serde(default)]
    date: Vec<String>,
    #[serde(default)]
    format: Vec<String>,
    #[serde(default)]
    creator: Vec<String>,
    #[serde(default)]
    language: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct GoogleBooksFeed {
    entry: Vec<GoogleBooksEntry>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum DataValue {
    Monolingual {
        label: String,
        language: String,
    },
    String(String),
    Entity(String),
    Date {
        time: String,
        precision: TimePrecision,
    },
    Quantity(i64),
}

impl DataValue {
    fn as_statement_value(&self) -> StatementValue {
        let svc = match self {
            DataValue::Monolingual { label, language } => StatementValueContent::MonolingualText {
                language: language.to_string(),
                text: label.to_string(),
            },
            DataValue::String(s) => StatementValueContent::String(s.to_string()),
            DataValue::Entity(e) => StatementValueContent::String(e.to_string()),
            DataValue::Date { time, precision } => StatementValueContent::Time {
                time: time.to_string(),
                precision: precision.to_owned(),
                calendarmodel: GREGORIAN_CALENDAR.to_string(),
            },
            DataValue::Quantity(amount) => StatementValueContent::Quantity {
                amount: format!("{amount}"),
                unit: "".to_string(),
            },
        };
        StatementValue::Value(svc)
    }
}

#[derive(Debug, Clone, Default, Hash, PartialEq, Eq)]
pub struct Reference {
    property: Option<String>,
    value: Option<String>,
    url: Option<String>,
}

impl Reference {
    fn prop(property: &str, value: &str) -> Self {
        Reference {
            property: Some(property.to_string()),
            value: Some(value.to_string()),
            url: None,
        }
    }

    fn none() -> Self {
        Reference {
            property: None,
            value: None,
            url: None,
        }
    }

    fn _url(url: &str) -> Self {
        Reference {
            property: None,
            value: None,
            url: Some(url.to_string()),
        }
    }

    fn is_equivalent(&self, reference: &wikibase_rest_api::Reference) -> bool {
        if let (Some(property), Some(value)) = (&self.property, &self.value) {
            reference.parts().iter().any(|prop_value| {
                let ref_prop = prop_value.property().id();
                let ref_value = match prop_value.value() {
                    StatementValue::Value(statement_value_content) => statement_value_content,
                    _ => return false,
                };
                let ref_value = match ref_value {
                    StatementValueContent::String(s) => s,
                    _ => return false,
                    // StatementValueContent::Time { time, precision, calendarmodel } => todo!(),
                    // StatementValueContent::Location { latitude, longitude, precision, globe } => todo!(),
                    // StatementValueContent::Quantity { amount, unit } => todo!(),
                    // StatementValueContent::MonolingualText { language, text } => todo!(),
                };
                property == ref_prop && value == ref_value
            })
        } else if let Some(url) = &self.url {
            reference.parts().iter().any(|prop_value| {
                let ref_prop = prop_value.property().id();
                let ref_value = match prop_value.value() {
                    StatementValue::Value(statement_value_content) => statement_value_content,
                    _ => return false,
                };
                let ref_value = match ref_value {
                    StatementValueContent::String(s) => s,
                    _ => return false,
                };
                ref_prop == "P854" && url == ref_value
            })
        } else {
            false
        }
    }

    fn as_ref_group(&self) -> Option<wikibase_rest_api::Reference> {
        let mut ret = wikibase_rest_api::Reference::default();
        if let (Some(property), Some(value)) = (&self.property, &self.value) {
            let p = PropertyType::new(
                property.to_owned(),
                Some(wikibase_rest_api::DataType::String),
            );
            let v = StatementValue::Value(StatementValueContent::String(value.to_owned()));
            let pv = PropertyValue::new(p, v);
            ret.parts_mut().push(pv);
        } else if let Some(url) = &self.url {
            let p = PropertyType::new("P854", Some(wikibase_rest_api::DataType::Url));
            let v = StatementValue::Value(StatementValueContent::String(url.to_owned()));
            let pv = PropertyValue::new(p, v);
            ret.parts_mut().push(pv);
        } else {
            return None;
        }

        let p = PropertyType::new("P813", Some(wikibase_rest_api::DataType::Time));
        let v = StatementValue::Value(StatementValueContent::Time {
            time: chrono::Utc::now().format("+%Y-%m-%dT00:00:00Z").to_string(),
            precision: TimePrecision::Day,
            calendarmodel: GREGORIAN_CALENDAR.to_string(),
        });
        let pv = PropertyValue::new(p, v);
        ret.parts_mut().push(pv);
        Some(ret)
    }
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
            isbn10: match isbn_10 {
                Some(isbn_array) => Isbn10::new(isbn_array).ok(),
                None => None,
            },
            isbn13: match isbn_13 {
                Some(isbn_array) => Isbn13::new(isbn_array).ok(),
                None => None,
            },
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
    fn isbn(&self) -> Option<String> {
        match self.isbn13 {
            Some(isbn) => Some(isbn.hyphenate().unwrap().to_string()),
            None => self
                .isbn10
                .map(|isbn| isbn.hyphenate().unwrap().to_string()),
        }
    }

    pub async fn retrieve(&mut self) -> Result<()> {
        let f1 = self.load_from_goodreads();
        let f2 = self.load_from_google_books();
        let _ = join!(f1, f2);
        Ok(())
    }

    async fn load_from_google_books(&self) -> Result<()> {
        let isbn = self
            .isbn()
            .ok_or_else(|| anyhow!("No ISBN found"))?
            .replace('-', "");
        let url =
            format!("https://books.google.com/books/feeds/volumes?q=isbn:{isbn}&max-results=25");

        let client = reqwest::Client::builder()
            .user_agent(
                "Mozilla/5.0 (Windows; U; Windows NT 5.1; rv:1.7.3) Gecko/20041001 Firefox/0.10.1",
            )
            // .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();
        let response = client.get(&url).send().await?;
        let xml = response.text().await?;
        self.parse_google_books_xml(&xml)
    }

    fn parse_google_books_xml(&self, xml: &str) -> Result<()> {
        let xml = xml
            .replace("<dc:title", "<dctitle")
            .replace("</dc:title", "</dctitle"); // To avoid XML namespace problems with serde

        let feed: GoogleBooksFeed = serde_xml_rs::from_str(&xml)?;
        // println!("{feed:#?}");

        let entry = feed
            .entry
            .first()
            .ok_or_else(|| anyhow!("No entry found in Google books"))?;

        let google_books_id = self.extract_google_book_identifiers(entry)?;

        if let Some(language) = entry.language.first() {
            self.add_reference(
                "P1476",
                DataValue::Monolingual {
                    label: entry.title.to_owned(),
                    language: language.to_owned(),
                },
                Reference::prop("P675", &google_books_id),
            );
        }

        for format in &entry.format {
            if let Some(captures) = RE_PAGES.captures(format.as_str()) {
                if let Some(first_group) = captures.get(1) {
                    if let Ok(number_of_pages) = first_group.as_str().parse::<i64>() {
                        self.add_reference(
                            "P1104",
                            DataValue::Quantity(number_of_pages),
                            Reference::prop("P675", &google_books_id),
                        );
                    }
                }
            }
            if format == "book" {
                self.add_reference(
                    "P31",
                    DataValue::Entity("Q571".to_string()),
                    Reference::prop("P675", &google_books_id),
                );
            }
        }

        for date in &entry.date {
            if let Some(captures) = RE_PAGES.captures(date.as_str()) {
                if let Some(first_group) = captures.get(1) {
                    let time = format!("+{}-01-01T00:00:00Z", first_group.as_str());
                    self.add_reference(
                        "P577",
                        DataValue::Date {
                            time,
                            precision: TimePrecision::Year,
                        },
                        Reference::prop("P675", &google_books_id),
                    );
                }
            }
        }

        for creator in &entry.creator {
            self.add_reference(
                "P225",
                DataValue::String(creator.to_owned()),
                Reference::prop("P675", &google_books_id),
            )
        }

        Ok(())
    }

    fn extract_google_book_identifiers(
        &self,
        entry: &GoogleBooksEntry,
    ) -> Result<String, anyhow::Error> {
        let mut google_books_id: Option<String> = None;
        for identifier in &entry.identifier {
            if let Some(captures) = RE_GOOGLE_BOOKS_ID.captures(identifier.as_str()) {
                if let Some(first_group) = captures.get(1) {
                    google_books_id = Some(first_group.as_str().to_string());
                }
            };
            if let Some(captures) = RE_ISBN_10.captures(identifier.as_str()) {
                if let Some(first_group) = captures.get(1) {
                    let isbn = first_group.as_str().to_string();
                    let isbn = format!(
                        "{}-{}-{}-{}",
                        &isbn[0..1],
                        &isbn[1..4],
                        &isbn[4..9],
                        &isbn[9..10]
                    );
                    self.add_reference("P957", DataValue::String(isbn), Reference::none());
                }
            };
            if let Some(captures) = RE_ISBN_13.captures(identifier.as_str()) {
                if let Some(first_group) = captures.get(1) {
                    let isbn = first_group.as_str().to_string();
                    let isbn = format!(
                        "{}-{}-{}-{}-{}",
                        &isbn[0..3],
                        &isbn[3..4],
                        &isbn[4..6],
                        &isbn[6..12],
                        &isbn[12..13]
                    );
                    self.add_reference("P212", DataValue::String(isbn), Reference::none());
                }
            };
        }
        let google_books_id = google_books_id.ok_or_else(|| anyhow!("No ID found"))?;
        self.add_reference(
            "P675",
            DataValue::String(google_books_id.clone()),
            Reference::none(),
        );
        Ok(google_books_id)
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
            )
        }

        for contributor in metadata.contributors {
            if contributor.role == "Author" {
                self.add_reference(
                    "P225",
                    DataValue::String(contributor.name.to_owned()),
                    Reference::prop("P8383", &goodreads_work_id),
                )
            }
        }

        if let Some(pages) = metadata.page_count {
            self.add_reference(
                "P1104",
                DataValue::Quantity(pages),
                Reference::prop("P8383", &goodreads_work_id),
            );
        }

        let language_code = match &metadata.language {
            Some(language) => match LANGUAGE_LABELS.get(language) {
                Some(code) => code,
                None => "",
            },
            None => "",
        };

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
                )
            }
        }

        Ok(())
    }

    fn add_reference(
        &self,
        property: &str, //&mut HashMap<DataValue, HashSet<Reference>>,
        value: DataValue,
        reference: Reference,
    ) {
        self.values
            .lock()
            .unwrap()
            .entry(property.to_string())
            .or_default()
            .entry(value)
            .or_default()
            .insert(reference);
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
            )
        }
        if let Some(isbn) = self.isbn13 {
            self.add_reference(
                "P212",
                DataValue::String(isbn.hyphenate().ok()?.to_string()),
                Reference::default(), // No reference for ISBN
            )
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
                self.add_new_references_to_statement(&mut statement, references);
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
                    Some(statement) => self.add_new_references_to_statement(statement, references),
                    None => {
                        let mut statement = Statement::default();
                        statement.new_id_for_entity(&entity_id);
                        statement.set_property(PropertyType::property(property.to_owned()));
                        statement.set_value(expected_value);
                        self.add_new_references_to_statement(&mut statement, references);
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

    fn add_new_references_to_statement(
        &self,
        statement: &mut Statement,
        references: &HashSet<Reference>,
    ) {
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
        let isbn_digits = isbn
            .chars()
            .filter_map(|c| c.to_digit(10))
            .map(|c| c as u8)
            .collect::<Vec<u8>>();
        isbn_digits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_google_books_xml() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let xml = include_str!("../test_files/google_books.xml");
        isbn2wiki.parse_google_books_xml(xml).unwrap();
        println!("{:?}", isbn2wiki.values);
        // TODO actually compare the parsed values with the expected values
    }
}
