use crate::isbn::ISBN2wiki;
use crate::reference::{DataValue, Reference};
use anyhow::{Result, anyhow};
use lazy_static::lazy_static;
use regex::Regex;
use serde::Deserialize;
use wikibase_rest_api::prelude::*;

lazy_static! {
    static ref RE_GOOGLE_BOOKS_ID: Regex = Regex::new(r"^([a-zA-Z0-9]+)$").unwrap();
    static ref RE_PAGES: Regex = Regex::new(r"^(\d+) pages$").unwrap();
    static ref RE_ISBN_10: Regex = Regex::new(r"^ISBN:(\d{9}[0-9X])$").unwrap();
    static ref RE_ISBN_13: Regex = Regex::new(r"^ISBN:(\d{12}[0-9X])$").unwrap();
}

#[derive(Debug, Deserialize, PartialEq)]
struct GoogleBooksEntry {
    id: Vec<String>,
    title: String,
    #[serde(default)]
    dc_identifier: Vec<String>,
    #[serde(default)]
    dc_title: Vec<String>,
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
pub struct GoogleBooksFeed {
    entry: Vec<GoogleBooksEntry>,
}

impl GoogleBooksFeed {
    pub async fn load_from_google_books(isbn2wiki: &ISBN2wiki) -> Result<()> {
        let isbn = isbn2wiki
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
            .build()?;
        let response = client.get(&url).send().await?;
        let xml = response.text().await?;
        Self::parse_google_books_xml(isbn2wiki, &xml)
    }

    fn parse_google_books_xml(isbn2wiki: &ISBN2wiki, xml: &str) -> Result<()> {
        let xml = xml.replace("<dc:", "<dc_").replace("</dc:", "</dc_"); // To avoid XML namespace problems with serde
        let feed: GoogleBooksFeed = serde_xml_rs::from_str(&xml)?; // Does not work properly
        let entry = feed
            .entry
            .first()
            .ok_or_else(|| anyhow!("No entry found in Google books"))?;

        let google_books_id = Self::extract_google_book_identifiers(isbn2wiki, entry)?;

        if let Some(language) = entry.language.first() {
            isbn2wiki.add_reference(
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
                        isbn2wiki.add_reference(
                            "P1104",
                            DataValue::Quantity(number_of_pages),
                            Reference::prop("P675", &google_books_id),
                        );
                    }
                }
            }
            if format == "book" {
                isbn2wiki.add_reference(
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
                    isbn2wiki.add_reference(
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
            isbn2wiki.add_reference(
                "P225",
                DataValue::String(creator.to_owned()),
                Reference::prop("P675", &google_books_id),
            );
        }

        Ok(())
    }

    fn extract_google_book_identifiers(
        isbn2wiki: &ISBN2wiki,
        entry: &GoogleBooksEntry,
    ) -> Result<String> {
        let mut google_books_id: Option<String> = None;
        for identifier in &entry.dc_identifier {
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
                    isbn2wiki.add_reference("P957", DataValue::String(isbn), Reference::none());
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
                    isbn2wiki.add_reference("P212", DataValue::String(isbn), Reference::none());
                }
            };
        }
        let google_books_id = google_books_id.ok_or_else(|| anyhow!("No ID found"))?;
        isbn2wiki.add_reference(
            "P675",
            DataValue::String(google_books_id.clone()),
            Reference::none(),
        );
        Ok(google_books_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_google_books_xml() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let xml = include_str!("../test_files/google_books.xml");
        let _ = GoogleBooksFeed::parse_google_books_xml(&isbn2wiki, xml);
        // TODO check results in isbn2wiki
    }
}
