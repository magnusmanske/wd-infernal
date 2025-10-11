use crate::{given_names::GivenNames, wikidata::Wikidata};
use axum::http::StatusCode;
use futures::future::join_all;
use lazy_static::lazy_static;
use mediawiki::Api;
use std::sync::Arc;
use tokio::sync::OnceCell;
use wikibase::{Reference, Snak, Statement};

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

    async fn get_given_names_for_gender(
        first_names: &[&str],
        api: &Api,
        gender: &str,
    ) -> Result<Vec<String>, StatusCode> {
        let futures: Vec<_> = first_names
            .iter()
            .map(|first_name| Wikidata::search_single_name(api, first_name, gender))
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

    // Not in use now, some error with the SPARQL in GivenNames
    async fn _add_first_names_gender_using_cached_given_names(
        first_names: Vec<&str>,
        statements: &mut Vec<Statement>,
    ) -> Result<(), StatusCode> {
        // let mut results = join_all([
        //     Self::get_given_names_for_gender(&first_names, api, "Q12308941"), // Male given name
        //     Self::get_given_names_for_gender(&first_names, api, "Q11879590"), // Female given name
        // ])
        // .await;
        // let mut female = results.pop().unwrap()?;
        // let mut male = results.pop().unwrap()?;
        // let both: Vec<_> = male
        //     .iter()
        //     .filter(|x| female.contains(x))
        //     .cloned()
        //     .collect();
        // male.retain(|x| !both.contains(x));
        // female.retain(|x| !both.contains(x));
        // // println!("Male: {male:?}\nFemale: {female:?}\nBoth: {both:?}");
        // let is_male = !male.is_empty();
        // let is_female = !female.is_empty();
        let gn = GivenNames::get_static().await;
        let is_male = first_names.iter().any(|x| gn.is_male(x));
        let is_female = first_names.iter().any(|x| gn.is_female(x));
        match (is_male, is_female) {
            (true, false) => statements.push(Self::gender_statement("Q6581097")), // male
            (false, true) => statements.push(Self::gender_statement("Q6581072")), // female
            _ => return Ok(()),
        }

        // Either male or female, no ambiguity
        let name_statements: Vec<_> = first_names
            .iter()
            .filter_map(|name| gn.name2qid(name))
            .map(|q| {
                let snak = Snak::new_item("P735", &format!("Q{q}"));
                let reference = Reference::new(vec![
                    Wikidata::infernal_reference_snak(),
                    Snak::new_item("P3452", "Q97033143"), // inferred from person's full name
                ]);
                Statement::new_normal(snak, vec![], vec![reference])
            })
            .collect();
        statements.extend(name_statements);
        Ok(())
    }

    async fn add_last_name(
        last_name: &str,
        api: &Api,
        statements: &mut Vec<Statement>,
    ) -> Result<(), StatusCode> {
        let results = Wikidata::search_single_name(api, last_name, "Q101352").await?;
        if results.len() == 1 {
            if let Some(entity) = results.first() {
                let snak = Snak::new_item("P734", entity);
                let reference = Reference::new(vec![
                    Wikidata::infernal_reference_snak(),
                    Snak::new_item("P3452", "Q97033143"), // inferred from person's full name
                ]);
                let statement = Statement::new_normal(snak, vec![], vec![reference]);
                statements.push(statement);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_name_gender() {
        let results = Person::name_gender("Heinrich Magnus Manske").await.unwrap();
        assert_eq!(results.len(), 4);
    }
}
