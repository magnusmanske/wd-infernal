use crate::wikidata::Wikidata;
use axum::http::StatusCode;
use futures::future::join_all;
use mediawiki::Api;
use std::collections::HashMap;
use std::sync::LazyLock;
use tokio::sync::RwLock;
use wikibase::{Reference, Snak, Statement};

/// Cache mapping (lowercase first name, P31 gender class Q-id) to matching Q-ids.
type NameGenderCache = HashMap<(String, String), Vec<String>>;

/// Cache for `search_single_name` results.
static NAME_GENDER_CACHE: LazyLock<RwLock<NameGenderCache>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Person;

impl Person {
    pub async fn name_gender(name: &str) -> Result<Vec<Statement>, StatusCode> {
        let mut statements = vec![];
        let mut parts = name.split_whitespace().collect::<Vec<_>>();
        let last_name = match parts.pop() {
            Some(last_name) => last_name,
            None => return Ok(statements), // No name, return empty set
        };
        let first_names = parts;
        let api = Wikidata::get_wikidata_api().await?;
        Self::add_last_name(last_name, &api, &mut statements).await?;
        Self::add_first_names_gender(first_names, &api, &mut statements).await?;
        Ok(statements)
    }

    /// Look up a single first name + gender class, using the cache when possible.
    async fn cached_search_single_name(
        api: &Api,
        first_name: &str,
        gender: &str,
    ) -> Result<Vec<String>, StatusCode> {
        let key = (first_name.to_lowercase(), gender.to_string());

        // Fast path: read lock
        {
            let cache = NAME_GENDER_CACHE.read().await;
            if let Some(cached) = cache.get(&key) {
                return Ok(cached.clone());
            }
        }

        // Cache miss — perform the actual lookup
        let result = Wikidata::search_single_name(api, first_name, gender).await?;

        // Store in cache
        {
            let mut cache = NAME_GENDER_CACHE.write().await;
            cache.insert(key, result.clone());
        }

        Ok(result)
    }

    async fn get_given_names_for_gender(
        first_names: &[&str],
        api: &Api,
        gender: &str,
    ) -> Result<Vec<String>, StatusCode> {
        let futures: Vec<_> = first_names
            .iter()
            .map(|first_name| Self::cached_search_single_name(api, first_name, gender))
            .collect();
        let results = join_all(futures).await;
        let mut items: Vec<String> = results
            .into_iter()
            .filter_map(|x| x.ok())
            .flatten()
            .collect();
        items.sort();
        items.dedup();
        Ok(items)
    }

    fn gender_statement(gender: &str) -> Statement {
        let snak = Snak::new_item("P21", gender);
        let reference = Reference::new(vec![
            Wikidata::infernal_reference_snak(),
            Snak::new_item("P3452", "Q69652498"), // inferred from person's given name
        ]);
        Statement::new_normal(snak, vec![], vec![reference])
    }

    async fn add_first_names_gender(
        first_names: Vec<&str>,
        api: &Api,
        statements: &mut Vec<Statement>,
    ) -> Result<(), StatusCode> {
        Self::add_first_names_gender_via_search(first_names, api, statements).await
    }

    async fn add_first_names_gender_via_search(
        first_names: Vec<&str>,
        api: &Api,
        statements: &mut Vec<Statement>,
    ) -> Result<(), StatusCode> {
        let mut results = join_all([
            Self::get_given_names_for_gender(&first_names, api, "Q12308941"), // Male given name
            Self::get_given_names_for_gender(&first_names, api, "Q11879590"), // Female given name
        ])
        .await;
        let mut female = results.pop().unwrap()?;
        let mut male = results.pop().unwrap()?;
        let both: Vec<_> = male
            .iter()
            .filter(|x| female.contains(x))
            .cloned()
            .collect();
        male.retain(|x| !both.contains(x));
        female.retain(|x| !both.contains(x));
        // println!("Male: {male:?}\nFemale: {female:?}\nBoth: {both:?}");
        let is_male = !male.is_empty();
        let is_female = !female.is_empty();
        match (is_male, is_female) {
            (true, false) => statements.push(Self::gender_statement("Q6581097")), // male
            (false, true) => statements.push(Self::gender_statement("Q6581072")), // female
            _ => {
                // Ignore
            }
        }
        if is_male != is_female {
            // Either male or female, no ambiguity
            let name_statements: Vec<_> = male
                .iter()
                .chain(female.iter())
                .map(|q| {
                    let snak = Snak::new_item("P735", q);
                    let reference = Reference::new(vec![
                        Wikidata::infernal_reference_snak(),
                        Snak::new_item("P3452", "Q97033143"), // inferred from person's full name
                    ]);
                    Statement::new_normal(snak, vec![], vec![reference])
                })
                .collect();
            statements.extend(name_statements);
        }
        Ok(())
    }

    async fn add_last_name(
        last_name: &str,
        api: &Api,
        statements: &mut Vec<Statement>,
    ) -> Result<(), StatusCode> {
        let results = Wikidata::search_single_name(api, last_name, "Q101352").await?;
        if let [entity] = results.as_slice() {
            let snak = Snak::new_item("P734", entity);
            let reference = Reference::new(vec![
                Wikidata::infernal_reference_snak(),
                Snak::new_item("P3452", "Q97033143"), // inferred from person's full name
            ]);
            let statement = Statement::new_normal(snak, vec![], vec![reference]);
            statements.push(statement);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: extract the item value (Q-id) from a statement's main snak
    fn snak_item_value(statement: &Statement) -> Option<String> {
        let dv = statement.main_snak().data_value().as_ref()?;
        if let wikibase::Value::Entity(ev) = dv.value() {
            Some(ev.id().to_string())
        } else {
            None
        }
    }

    #[tokio::test]
    async fn test_name_gender_male() {
        // "Heinrich Magnus Manske" — two male given names + last name + gender
        let results = Person::name_gender("Heinrich Magnus Manske").await.unwrap();
        assert_eq!(
            results.len(),
            4,
            "Expected 4 statements (last name + gender + 2 given names)"
        );

        // First statement should be last name (P734)
        assert_eq!(results[0].main_snak().property(), "P734");

        // Second statement should be gender (P21) = male (Q6581097)
        assert_eq!(results[1].main_snak().property(), "P21");
        assert_eq!(snak_item_value(&results[1]).as_deref(), Some("Q6581097"));

        // Remaining statements should be given names (P735)
        for s in &results[2..] {
            assert_eq!(s.main_snak().property(), "P735");
        }
    }

    #[tokio::test]
    async fn test_name_gender_female() {
        // "Elisabeth Manske" — a clearly female first name
        let results = Person::name_gender("Elisabeth Manske").await.unwrap();
        // Should contain a gender statement for female
        let gender_statements: Vec<_> = results
            .iter()
            .filter(|s| s.main_snak().property() == "P21")
            .collect();
        assert_eq!(
            gender_statements.len(),
            1,
            "Expected exactly one gender statement"
        );
        assert_eq!(
            snak_item_value(gender_statements[0]).as_deref(),
            Some("Q6581072"),
            "Expected female gender Q6581072"
        );
    }

    #[tokio::test]
    async fn test_name_gender_empty() {
        // Empty string: no name parts at all
        let results = Person::name_gender("").await.unwrap();
        assert!(
            results.is_empty(),
            "Empty name should produce no statements"
        );
    }

    #[tokio::test]
    async fn test_name_gender_single_word() {
        // Single word is treated as last name only, no first names
        let results = Person::name_gender("Manske").await.unwrap();
        // Should have at most a last name statement (P734), no gender
        let gender_statements: Vec<_> = results
            .iter()
            .filter(|s| s.main_snak().property() == "P21")
            .collect();
        assert!(
            gender_statements.is_empty(),
            "Single-word name should not produce a gender statement"
        );
    }

    #[tokio::test]
    async fn test_name_gender_references() {
        // Verify that every statement has at least one reference containing the infernal snak (P887)
        let results = Person::name_gender("Heinrich Manske").await.unwrap();
        assert!(!results.is_empty());
        for statement in &results {
            let refs = statement.references();
            assert!(!refs.is_empty(), "Every statement should have a reference");
            let has_infernal = refs
                .iter()
                .any(|r| r.snaks().iter().any(|sn| sn.property() == "P887"));
            assert!(
                has_infernal,
                "Every reference should contain the infernal snak P887"
            );
        }
    }

    #[tokio::test]
    async fn test_name_gender_consistent_calls() {
        // Calling twice with the same input should yield the same result
        let r1 = Person::name_gender("Heinrich Manske").await.unwrap();
        let r2 = Person::name_gender("Heinrich Manske").await.unwrap();
        assert_eq!(
            r1.len(),
            r2.len(),
            "Repeated calls should return same number of statements"
        );
        for (a, b) in r1.iter().zip(r2.iter()) {
            assert_eq!(a.main_snak().property(), b.main_snak().property());
            assert_eq!(snak_item_value(a), snak_item_value(b));
        }
    }

    #[tokio::test]
    async fn test_name_gender_has_given_name_statements() {
        // For an unambiguous male name, given name (P735) statements should be present
        let results = Person::name_gender("Heinrich Manske").await.unwrap();
        let given_name_stmts: Vec<_> = results
            .iter()
            .filter(|s| s.main_snak().property() == "P735")
            .collect();
        assert!(
            !given_name_stmts.is_empty(),
            "Expected at least one given name statement"
        );
    }
}
