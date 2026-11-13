use crate::reference::{DataValue, Reference, ValueMatch};
use anyhow::{Result, anyhow};
use bookmeta::{Book, BookSources, Isbn, Source};
use reqwest::Client;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use wikibase_rest_api::prelude::*;
use wikimisc::entity_id::ItemId;

const USER_AGENT: &str = wikimisc::user_agent!(
    name = "WikidataInfernal",
    "https://wd-infernal.toolforge.org/; mailto:magnusmanske@toolforge.org"
);

/// Shared by the ISBN metadata sources.
static HTTP_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    wikimisc::http::client_builder(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to build ISBN HTTP client")
});

/// The catalogues queried for book metadata.
///
/// `bookmeta` can query more, but each enabled source is another outgoing
/// request per lookup. Enable another source here and map its ID to a Wikidata
/// property in [`ISBN2wiki::source_id`].
static BOOK_SOURCES: LazyLock<BookSources> = LazyLock::new(|| {
    BookSources::new(HTTP_CLIENT.clone())
        .with_timeout(Duration::from_secs(30))
        .without_all([
            Source::Dnb,
            Source::K10plus,
            Source::Loc,
            Source::Bnf,
            Source::Bne,
            Source::Kb,
            Source::Ndl,
            Source::Crossref,
        ])
});

#[derive(Debug)]
pub struct ISBN2wiki {
    isbn: Isbn,
    pub values: Mutex<HashMap<String, HashMap<DataValue, HashSet<Reference>>>>,
    /// The item's current statements, when created from an item; patches are
    /// generated against these, so existing values get references, not duplicates.
    existing: Statements,
    /// Wikidata items that sources say already represent this edition.
    existing_items: Mutex<BTreeSet<String>>,
}

impl ISBN2wiki {
    pub fn new(isbn: &str) -> Option<Self> {
        let ret = Self::with_isbn(Isbn::parse(isbn).ok()?);
        ret.add_isbn_values_as_statements();
        Some(ret)
    }

    pub async fn new_from_item(item_id: &str) -> Option<Self> {
        let item: ItemId = item_id.parse().ok()?;
        let entity_id = EntityId::item(item.to_string());
        let api = RestApi::wikidata().ok()?;
        let statements = Statements::get(&entity_id, &api).await.ok()?;
        let isbn = Self::extract_isbn(&statements)?;

        let ret = Self {
            existing: statements,
            ..Self::with_isbn(isbn)
        };
        ret.add_isbn_values_as_statements();

        Some(ret)
    }

    fn with_isbn(isbn: Isbn) -> Self {
        Self {
            isbn,
            values: Mutex::default(),
            existing: Statements::default(),
            existing_items: Mutex::default(),
        }
    }

    fn extract_isbn(statements: &Statements) -> Option<Isbn> {
        ["P212", "P957"].into_iter().find_map(|property| {
            statements
                .statements()
                .iter()
                .filter(|(prop, _statements)| *prop == property)
                .filter_map(|(_prop, statements)| statements.first())
                .find_map(|statement| match statement.value() {
                    StatementValue::Value(StatementValueContent::String(value)) => {
                        Isbn::parse(value).ok()
                    }
                    _ => None,
                })
        })
    }

    /// Queries all sources. Succeeds if at least one of them returned data, so
    /// that one source lacking the book (or being down) does not discard the
    /// others' data.
    pub async fn retrieve(&self) -> Result<()> {
        let lookup = BOOK_SOURCES.lookup(&self.isbn).await;
        for failure in &lookup.failures {
            tracing::info!(
                "{} lookup failed: {}",
                failure.source.label(),
                failure.message
            );
        }
        for book in &lookup.books {
            self.add_book(book);
        }
        for item in &lookup.wikidata {
            self.add_existing_item(&item.id);
        }
        if lookup.books.is_empty() && lookup.wikidata.is_empty() {
            return Err(anyhow!("No source returned metadata"));
        }
        Ok(())
    }

    /// The source's own identifier for the edition, and the Wikidata property
    /// it belongs in. It references the source's other statements.
    fn source_id(book: &Book) -> Option<(&'static str, &str)> {
        match book.source {
            Source::GoogleBooks => book.ids.google_books.as_deref().map(|id| ("P675", id)),
            Source::OpenLibrary => book.ids.open_library.as_deref().map(|id| ("P648", id)),
            Source::Goodreads => book.ids.goodreads.as_deref().map(|id| ("P2969", id)),
            _ => None,
        }
    }

    fn add_book(&self, book: &Book) {
        let Some((id_property, id)) = Self::source_id(book) else {
            return;
        };
        self.add_reference(
            id_property,
            DataValue::String(id.to_string()),
            Reference::none(),
        );
        let reference = Reference::prop(id_property, id);

        self.add_reference(
            "P31",
            DataValue::Entity("Q571".to_string()),
            reference.clone(),
        );

        if let Some(language) = &book.language {
            if !book.title.trim().is_empty() {
                self.add_reference(
                    "P1476",
                    DataValue::Monolingual {
                        label: book.title.clone(),
                        language: language.clone(),
                    },
                    reference.clone(),
                );
            }
            if let Some(subtitle) = book
                .subtitle
                .as_deref()
                .map(str::trim)
                .filter(|subtitle| !subtitle.is_empty())
            {
                self.add_reference(
                    "P1680",
                    DataValue::Monolingual {
                        label: subtitle.to_string(),
                        language: language.clone(),
                    },
                    reference.clone(),
                );
            }
        }

        for author in &book.authors {
            self.add_reference(
                "P2093",
                DataValue::String(author.clone()),
                reference.clone(),
            );
        }

        if let Some(pages) = book.pages {
            self.add_reference(
                "P1104",
                DataValue::Quantity(i64::from(pages)),
                reference.clone(),
            );
        }

        if let Some(year) = DataValue::year(&book.year) {
            self.add_reference("P577", year, reference);
        }
    }

    fn add_existing_item(&self, item_id: &str) {
        if item_id.parse::<ItemId>().is_ok() {
            let mut items = self
                .existing_items
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            items.insert(item_id.to_string());
        }
    }

    pub fn existing_items(&self) -> Vec<String> {
        let items = self
            .existing_items
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        items.iter().cloned().collect()
    }

    pub fn add_reference(&self, property: &str, value: DataValue, reference: Reference) {
        // Recover from mutex poisoning so data is not lost when a thread
        // panics while holding the lock.
        let mut values = self.values.lock().unwrap_or_else(|e| e.into_inner());
        values
            .entry(property.to_string())
            .or_default()
            .entry(value)
            .or_default()
            .insert(reference);
    }

    fn add_isbn_values_as_statements(&self) {
        self.add_reference(
            "P212",
            DataValue::String(self.isbn.isbn13_hyphenated()),
            Reference::none(), // No reference for ISBN
        );
        if let Some(isbn10) = self.isbn.isbn10_hyphenated() {
            self.add_reference(
                "P957",
                DataValue::String(isbn10),
                Reference::none(), // No reference for ISBN
            );
        }
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
                    .or_default()
                    .push(statement);
            }
        }

        // TODO labels etc
        Ok(ret)
    }

    pub fn generate_patch(&self, item_id: &str) -> Result<impl Patch> {
        EntityId::new(item_id)?; // Validates the ID
        let mut statements_new = self.existing.clone();
        let values = self
            .values
            .lock()
            .map_err(|_| anyhow!("Values lock poisoned"))?;

        for (property, dv2refs) in values.iter() {
            for (datavalue, references) in dv2refs {
                let mut statements: Vec<&mut Statement> = vec![];
                let mut stated_more_precisely = false;
                if let Some(existing) = statements_new.statements_mut().get_mut(property) {
                    for statement in existing.iter_mut() {
                        match datavalue.compare(statement.value()) {
                            ValueMatch::Same => statements.push(statement),
                            ValueMatch::MorePrecise => stated_more_precisely = true,
                            ValueMatch::Different => {}
                        }
                    }
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
                    // A less precise duplicate would only add noise
                    None if stated_more_precisely => {}
                    None => {
                        let mut statement = Statement::default();
                        statement.set_property(PropertyType::property(property.to_owned()));
                        statement.set_value(datavalue.as_statement_value());
                        Self::add_new_references_to_statement(&mut statement, references);
                        drop(statements);
                        statements_new
                            .statements_mut()
                            .entry(property.to_owned())
                            .or_default()
                            .push(statement);
                    }
                }
            }
        }

        let patch = statements_new.patch(&self.existing)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use wikibase_rest_api::prelude::TimePrecision;

    fn book(source: Source) -> Book {
        let mut book = Book::new(source);
        book.title = "A title".to_string();
        book.subtitle = Some("A subtitle".to_string());
        book.authors = vec!["An author".to_string()];
        book.year = "1988".to_string();
        book.language = Some("en".to_string());
        book.pages = Some(96);
        book
    }

    fn values(isbn2wiki: &ISBN2wiki) -> HashMap<String, Vec<DataValue>> {
        isbn2wiki
            .values
            .lock()
            .unwrap()
            .iter()
            .map(|(property, values)| (property.clone(), values.keys().cloned().collect()))
            .collect()
    }

    #[tokio::test]
    async fn test_new_from_item_rejects_bad_item() {
        for item in ["Q1/../../x", "P957", ""] {
            assert!(ISBN2wiki::new_from_item(item).await.is_none(), "{item}");
        }
    }

    // ── ISBN2wiki::new ────────────────────────────────────────────────────────

    #[test]
    fn test_new_valid_isbn13() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").expect("valid ISBN-13");
        assert_eq!(isbn2wiki.isbn.isbn13_digits(), "9782267027006");
        assert_eq!(isbn2wiki.isbn.isbn13_hyphenated(), "978-2-267-02700-6");
    }

    #[test]
    fn test_new_valid_isbn10_normalises_to_both_forms() {
        let isbn2wiki = ISBN2wiki::new("2267027003").expect("valid ISBN-10");
        assert_eq!(isbn2wiki.isbn.isbn13_digits(), "9782267027006");
        assert_eq!(
            isbn2wiki.isbn.isbn10_digits().as_deref(),
            Some("2267027003")
        );
    }

    #[test]
    fn test_new_hyphenated_isbn13() {
        let isbn2wiki = ISBN2wiki::new("978-2-267-02700-6").expect("hyphenated ISBN-13");
        assert_eq!(isbn2wiki.isbn.isbn13_digits(), "9782267027006");
    }

    #[test]
    fn test_new_invalid_returns_none() {
        // Bad check digit. (A checksum-valid but unregistered ISBN, such as
        // all zeros, is accepted with its digits as the display form.)
        assert!(
            ISBN2wiki::new("9780140328722").is_none(),
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
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let values = values(&isbn2wiki);
        let isbn = Isbn::parse("9782267027006").unwrap();
        assert_eq!(
            values["P212"],
            [DataValue::String(isbn.isbn13_hyphenated())]
        );
        assert_eq!(
            values["P957"],
            [DataValue::String(isbn.isbn10_hyphenated().unwrap())]
        );
    }

    // ── extract_isbn ──────────────────────────────────────────────────────────

    #[test]
    fn test_extract_isbn_reads_p212() {
        let statement = |property: &str, value: &str| {
            serde_json::json!({
                "id": "Q1$1",
                "property": {"id": property},
                "value": {"type": "value", "content": value},
                "rank": "normal",
                "qualifiers": [],
                "references": []
            })
        };
        let statements = Statements::from_json(&serde_json::json!({
            "P212": [statement("P212", "978-2-267-02700-6")],
        }))
        .unwrap();
        let isbn = ISBN2wiki::extract_isbn(&statements).unwrap();
        assert_eq!(isbn.isbn13_digits(), "9782267027006");
    }

    #[test]
    fn test_extract_isbn_falls_back_to_p957() {
        let statement = |property: &str, value: &str| {
            serde_json::json!({
                "id": "Q1$1",
                "property": {"id": property},
                "value": {"type": "value", "content": value},
                "rank": "normal",
                "qualifiers": [],
                "references": []
            })
        };
        let statements = Statements::from_json(&serde_json::json!({
            "P957": [statement("P957", "2-267-02700-3")],
        }))
        .unwrap();
        let isbn = ISBN2wiki::extract_isbn(&statements).unwrap();
        assert_eq!(isbn.isbn13_digits(), "9782267027006");
    }

    // ── source_id / add_book ──────────────────────────────────────────────────

    #[test]
    fn test_source_id_maps_enabled_sources() {
        let mut google = book(Source::GoogleBooks);
        google.ids.google_books = Some("abc".to_string());
        assert_eq!(ISBN2wiki::source_id(&google), Some(("P675", "abc")));

        let mut open_library = book(Source::OpenLibrary);
        open_library.ids.open_library = Some("OL1M".to_string());
        assert_eq!(ISBN2wiki::source_id(&open_library), Some(("P648", "OL1M")));

        let mut goodreads = book(Source::Goodreads);
        goodreads.ids.goodreads = Some("123".to_string());
        assert_eq!(ISBN2wiki::source_id(&goodreads), Some(("P2969", "123")));

        assert_eq!(ISBN2wiki::source_id(&book(Source::Dnb)), None);
    }

    #[test]
    fn test_add_book_maps_metadata() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let mut google = book(Source::GoogleBooks);
        google.ids.google_books = Some("abc".to_string());
        isbn2wiki.add_book(&google);

        let values = values(&isbn2wiki);
        assert_eq!(values["P675"], [DataValue::String("abc".to_string())]);
        assert_eq!(values["P31"], [DataValue::Entity("Q571".to_string())]);
        assert_eq!(
            values["P1476"],
            [DataValue::Monolingual {
                label: "A title".to_string(),
                language: "en".to_string(),
            }]
        );
        assert_eq!(
            values["P1680"],
            [DataValue::Monolingual {
                label: "A subtitle".to_string(),
                language: "en".to_string(),
            }]
        );
        assert_eq!(
            values["P2093"],
            [DataValue::String("An author".to_string())]
        );
        assert_eq!(values["P1104"], [DataValue::Quantity(96)]);
        assert_eq!(
            values["P577"],
            [DataValue::Date {
                time: "+1988-00-00T00:00:00Z".to_string(),
                precision: TimePrecision::Year,
            }]
        );
    }

    #[test]
    fn test_add_book_references_metadata_by_source_id() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let mut google = book(Source::GoogleBooks);
        google.ids.google_books = Some("abc".to_string());
        isbn2wiki.add_book(&google);

        let values = isbn2wiki.values.lock().unwrap();
        let title = DataValue::Monolingual {
            label: "A title".to_string(),
            language: "en".to_string(),
        };
        assert_eq!(
            values["P1476"][&title],
            HashSet::from([Reference::prop("P675", "abc")])
        );
    }

    #[test]
    fn test_add_book_ignores_sources_without_id_mapping() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let mut dnb = book(Source::Dnb);
        dnb.ids.dnb = Some("123".to_string());
        isbn2wiki.add_book(&dnb);

        let values = values(&isbn2wiki);
        // Only the two ISBN statements from `new`.
        assert_eq!(values.len(), 2);
        assert!(!values.contains_key("P31"));
    }

    #[test]
    fn test_add_book_needs_a_language_for_titles() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let mut google = book(Source::GoogleBooks);
        google.ids.google_books = Some("abc".to_string());
        google.language = None;
        isbn2wiki.add_book(&google);

        let values = values(&isbn2wiki);
        assert!(!values.contains_key("P1476"));
        assert!(!values.contains_key("P1680"));
        assert!(values.contains_key("P2093"));
    }

    // ── existing_items ────────────────────────────────────────────────────────

    #[test]
    fn test_add_existing_item_validates() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        isbn2wiki.add_existing_item("Q1234");
        isbn2wiki.add_existing_item("not an item");
        assert_eq!(isbn2wiki.existing_items(), ["Q1234"]);
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

    // ── generate_patch ───────────────────────────────────────────────────────

    #[test]
    fn test_generate_patch_produces_valid_patch() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let patch = isbn2wiki.generate_patch("Q1234");
        assert!(
            patch.is_ok(),
            "generate_patch should succeed for valid item ID"
        );
    }

    #[test]
    fn test_generate_patch_invalid_entity_id() {
        let isbn2wiki = ISBN2wiki::new("9782267027006").unwrap();
        let patch = isbn2wiki.generate_patch("not-a-valid-id");
        assert!(
            patch.is_err(),
            "generate_patch should fail for invalid entity ID"
        );
    }

    #[test]
    fn test_generate_patch_against_existing_statements() {
        let statement = |id: &str, property: &str, content: serde_json::Value| {
            serde_json::json!({
                "id": format!("Q1${id}"),
                "property": {"id": property},
                "value": {"type": "value", "content": content},
                "rank": "normal",
                "qualifiers": [],
                "references": []
            })
        };
        let existing = Statements::from_json(&serde_json::json!({
            "P212": [statement("0", "P212", "978-84-226-4891-8".into())],
            "P957": [statement("1", "P957", "84-226-4891-1".into())],
            "P1104": [statement("2", "P1104", serde_json::json!({"amount": "+515", "unit": "1"}))],
            "P577": [statement("3", "P577", serde_json::json!({
                "time": "+2014-10-00T00:00:00Z",
                "precision": 10,
                "calendarmodel": GREGORIAN_CALENDAR
            }))],
        }))
        .unwrap();
        let isbn2wiki = ISBN2wiki {
            existing,
            ..ISBN2wiki::new("84-226-4891-1").unwrap()
        };
        let source = Reference::prop("P648", "OL1M");
        let year_2014 = DataValue::Date {
            time: "+2014-01-01T00:00:00Z".to_string(),
            precision: TimePrecision::Year,
        };
        isbn2wiki.add_reference("P1104", DataValue::Quantity(515), source.clone());
        isbn2wiki.add_reference("P577", year_2014, source.clone());
        isbn2wiki.add_reference("P648", DataValue::String("OL1M".to_string()), source);

        let patch = isbn2wiki.generate_patch("Q1").unwrap();
        let mut ops: Vec<_> = patch
            .patch()
            .iter()
            .map(|op| {
                let op = serde_json::to_value(op).unwrap();
                format!(
                    "{} {}",
                    op["op"].as_str().unwrap(),
                    op["path"].as_str().unwrap()
                )
            })
            .collect();
        ops.sort();
        // Existing ISBNs: untouched. Same page count: referenced. Year of an
        // existing month: skipped. New ID: added.
        assert_eq!(
            ops,
            [
                "add /statements/P1104/0/references/0",
                "add /statements/P648/-",
            ]
        );
    }
}
