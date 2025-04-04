use anyhow::{anyhow, Result};
use grscraper::MetadataRequestBuilder;
use isbn::{Isbn10, Isbn13};
use lazy_static::lazy_static;
use regex::Regex;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;

lazy_static! {
    static ref GOODREADS_ID: Regex = Regex::new(r"/(\d+)\.jpg$").unwrap();
    static ref GOOGLE_BOOKS_ID: Regex = Regex::new(r"^([a-zA-Z0-9]+)$").unwrap();
    static ref LANGUAGE_LABELS: HashMap<String, String> = {
        let json_string = include_str!("../static/languages.json");
        let data: HashMap<String, String> = serde_json::from_str(json_string).unwrap();
        data
    };
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum DataValue {
    Monolingual { label: String, language: String },
    String(String),
    Date { time: String, precision: u8 },
    Quantity(i64),
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

    fn _url(url: &str) -> Self {
        Reference {
            property: None,
            value: None,
            url: Some(url.to_string()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ISBN2wiki {
    pub isbn10: Option<Isbn10>,
    pub isbn13: Option<Isbn13>,
    pub values: HashMap<String, HashMap<DataValue, HashSet<Reference>>>,
}

impl ISBN2wiki {
    pub fn new(isbn: &str) -> Option<Self> {
        let isbn_digits = isbn
            .chars()
            .filter_map(|c| c.to_digit(10))
            .map(|c| c as u8)
            .collect::<Vec<u8>>();
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
        // let _ = self.load_from_goodreads().await; // Ignore errors
        self.load_from_google_books().await?;
        Ok(())
    }

    async fn load_from_google_books(&mut self) -> Result<()> {
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

    fn parse_google_books_xml(&mut self, xml: &str) -> Result<()> {
        let xml = xml
            .replace("<dc:title", "<dctitle")
            .replace("</dc:title", "</dctitle"); // To avoid XML namespace problems with serde
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

        let feed: GoogleBooksFeed = serde_xml_rs::from_str(&xml)?;
        println!("{feed:#?}");

        let entry = feed
            .entry
            .first()
            .ok_or_else(|| anyhow!("No entry found in Google books"))?;

        let mut google_books_id: Option<String> = None;
        for identifier in &entry.identifier {
            if let Some(captures) = GOOGLE_BOOKS_ID.captures(identifier.as_str()) {
                if let Some(first_group) = captures.get(1) {
                    google_books_id = Some(first_group.as_str().to_string());
                }
            };
        }
        let google_books_id = google_books_id.ok_or_else(|| anyhow!("No ID found"))?;
        println!("Google Books ID: {google_books_id}");

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

        Ok(())
    }

    async fn _load_from_goodreads(&mut self) -> Result<()> {
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
        let goodreads_work_id = match GOODREADS_ID.captures(goodreads_thumbnail_url.as_str()) {
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
                9
            } else {
                11
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
        &mut self,
        property: &str, //&mut HashMap<DataValue, HashSet<Reference>>,
        value: DataValue,
        reference: Reference,
    ) {
        self.values
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_google_books_xml() {
        let mut isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let xml = include_str!("../static/google_books.xml");
        isbn2wiki.parse_google_books_xml(xml).unwrap();
        println!("{:?}", isbn2wiki.values);
    }
}
