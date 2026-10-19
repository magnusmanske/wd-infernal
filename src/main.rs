#![forbid(unsafe_code)]
#![allow(clippy::collapsible_if)]
#![warn(
    clippy::cognitive_complexity,
    clippy::dbg_macro,
    clippy::debug_assert_with_mut_call,
    clippy::doc_link_with_quotes,
    clippy::doc_markdown,
    clippy::empty_line_after_outer_attr,
    clippy::empty_structs_with_brackets,
    clippy::float_cmp,
    clippy::float_cmp_const,
    clippy::float_equality_without_abs,
    keyword_idents,
    clippy::missing_const_for_fn,
    missing_copy_implementations,
    missing_debug_implementations,
    // clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::mod_module_files,
    non_ascii_idents,
    noop_method_call,
    // clippy::option_if_let_else,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::semicolon_if_nothing_returned,
    clippy::unseparated_literal_suffix,
    clippy::shadow_unrelated,
    // clippy::similar_names,
    clippy::suspicious_operation_groupings,
    unused_crate_dependencies,
    unused_extern_crates,
    unused_import_braces,
    clippy::unused_self,
    clippy::use_debug,
    clippy::used_underscore_binding,
    clippy::useless_let_if_seq,
    // clippy::wildcard_dependencies,
    clippy::wildcard_imports
)]

/// mimalloc (in secure mode) let RSS creep up across identical referee
/// workloads, keeping memory resident after the entity cache was cleared.
/// jemalloc's peaks stayed flat, and its background threads return freed
/// memory to the OS.
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use serde_json::json;
use std::fs::File;
use wikibase_rest_api::Patch as _;

pub mod change_wiki;
pub mod crosscats;
pub mod database;
pub mod given_names;
pub mod google_books;
pub mod initial_search;
pub mod isbn;
pub mod location;
pub mod metrics;
pub mod open_library;
pub mod person;
pub mod referee;
pub mod reference;
pub mod server;
pub mod viaf;
pub mod wikidata;

/// The parsed `config.json`, or `None` if there is no such file.
pub fn load_config() -> serde_json::Result<Option<serde_json::Value>> {
    let Ok(file) = File::open("config.json")
        .or_else(|_| File::open("/data/project/wd-infernal/wd-infernal/config.json"))
    else {
        return Ok(None);
    };
    serde_json::from_reader(std::io::BufReader::new(file)).map(Some)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    #![allow(clippy::use_debug, clippy::print_stdout)]
    if std::env::args().len() > 2 {
        let command = std::env::args().nth(1).unwrap();
        match command.as_str() {
            "viaf" => {
                let query = std::env::args().nth(2).unwrap();
                let result = viaf::search_viaf_for_local_names(&query).await.unwrap();
                println!("{result:#?}");
            }
            "isbn" => {
                let item_id = std::env::args().nth(2).unwrap();
                let mut isbn2wiki = isbn::ISBN2wiki::new_from_item(&item_id).await.unwrap();
                isbn2wiki.retrieve().await.unwrap();
                let patch = isbn2wiki.generate_patch(&item_id).unwrap();
                println!("{}", json!(patch.patch()));
            }
            "name_gender" => {
                let item_id = std::env::args().nth(2).unwrap();
                let statements = person::Person::name_gender(&item_id).await.unwrap();
                println!("{statements:?}");
            }
            "referee" => {
                let item = std::env::args().nth(2).unwrap();
                let ret = referee::Referee::new()
                    .await
                    .unwrap()
                    .get_potential_references(&item)
                    .await
                    .unwrap();
                println!("{ret:#?}");
                println!("{}", ret.len());
            }
            "crosscats" => {
                let item = std::env::args().nth(2).unwrap();
                let depth: u32 = std::env::args()
                    .nth(3)
                    .unwrap_or_else(|| "0".to_string())
                    .parse()
                    .unwrap();
                let language = std::env::args().nth(4).unwrap_or_else(|| "en".to_string());
                let ret = crosscats::CrossCats::cross_cats(&item, depth, &language)
                    .await
                    .unwrap();
                println!("{ret:#?}");
            }
            "initial_search" => {
                let query = std::env::args().nth(2).unwrap();
                let ret = initial_search::InitialSearch::run(&query).await.unwrap();
                println!("{ret:#?}");
            }
            other => {
                println!("{other} not implemented in main");
            }
        }
        Ok(())
    } else {
        server::Server::start().await
    }
}
