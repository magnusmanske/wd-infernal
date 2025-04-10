use anyhow::{anyhow, Result};
use chrono::prelude::*;
use futures::future::join_all;
use futures::join;
use lazy_static::lazy_static;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};
use wikibase::{
    entity_container::EntityContainer, mediawiki::Api, DataValueType, Entity, EntityTrait, Snak,
    SnakDataType, Statement,
};

lazy_static! {
    static ref RE_WIKI: Regex = Regex::new(r"\b(wikipedia|wikimedia|wik[a-z-]+)\.org/").unwrap();
}

const BAD_URLS: &[&str] = &[
    "://g.co/",
    "viaf.org/",
    "wmflabs.org",
    "www.google.com",
    "toolforge.org",
];

type UniqueUrlCandidates = HashMap<String, UrlCandidate>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum UrlType {
    WikiExternal,
    ExternalId,
    DirectWebsite,
}

#[derive(Debug, Serialize, Deserialize)]
struct UrlPatternBlacklist {
    id: usize,
    pattern: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Url {
    id: usize,
    url: String,
    server: String,
    timestamp: i64,
    status: String,
    contents: Option<String>,
    content_format: Option<String>,
}

#[derive(Debug, Serialize)]
struct EntityStatement {
    entity: String,
    property: String,
    id: String,
    claim: Statement,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UrlCandidate {
    url: String,
    url_type: UrlType,
    property: Option<String>,
    external_id: Option<String>,
    stated_in: Option<String>,
    language: String,
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, PartialOrd, Eq, Ord)]
pub struct TextPart {
    before: String,
    regexp_match: String,
    after: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConciseUrlCandidate {
    statement_id: String,
    url: String,
    property: Option<String>,
    external_id: Option<String>,
    stated_in: Option<String>,
    language: String,
    texts: Vec<TextPart>,
}

impl Ord for ConciseUrlCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.statement_id
            .cmp(&other.statement_id)
            .then(self.url.cmp(&other.url))
            .then(self.property.cmp(&other.property))
            .then(self.external_id.cmp(&other.external_id))
            .then(self.language.cmp(&other.language))
    }
}

impl PartialOrd for ConciseUrlCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for ConciseUrlCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.statement_id == other.statement_id
            && self.url == other.url
            && self.property == other.property
            && self.external_id == other.external_id
            && self.language == other.language
    }
}

impl Eq for ConciseUrlCandidate {}

impl ConciseUrlCandidate {
    fn new(statement_id: &str, uc: &UrlCandidate, tp: &TextPart) -> Self {
        Self {
            statement_id: statement_id.to_string(),
            url: uc.url.clone(),
            property: uc.property.clone(),
            external_id: uc.external_id.clone(),
            stated_in: uc.stated_in.clone(),
            language: uc.language.clone(),
            texts: vec![tp.clone()],
        }
    }
}

pub struct Referee {
    api: Api,
    entities: EntityContainer,
    no_refs_for_properties: HashSet<String>,
    unsupported_entity_markers: Vec<(String, String)>,
    client: Client,
}

impl Referee {
    pub async fn new() -> Result<Self> {
        let no_refs = vec!["P225", "P373", "P1472", "P1889"]
            .into_iter()
            .map(String::from)
            .collect();

        let unsupported = vec![
            ("P31", "Q13442814"), // Scholarly article
            ("P31", "Q16521"),    // Taxon
            ("P31", "Q4167836"),  // category
            ("P31", "Q4167410"),  // disambiguation page
            ("P31", "Q5296"),     // main page
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();

        let client = Client::builder()
            .user_agent(
                "Mozilla/5.0 (Windows; U; Windows NT 5.1; rv:1.7.3) Gecko/20041001 Firefox/0.10.1",
            )
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();

        Ok(Self {
            api: Api::new("https://www.wikidata.org/w/api.php").await?,
            entities: EntityContainer::new(),
            no_refs_for_properties: no_refs,
            unsupported_entity_markers: unsupported,
            client,
        })
    }

    fn validate_url(url: &str) -> Result<()> {
        for bad_url in BAD_URLS {
            if url.contains(bad_url) {
                return Err(anyhow!("Bad URL"));
            }
        }
        Ok(())
    }

    async fn load_contents_from_url(&self, url: &str) -> Result<String> {
        Self::validate_url(url)?;
        let url = url
            .replace("&amp;", "&")
            .trim()
            .to_string()
            .replace(" ", "%20");

        let response = self.client.get(&url).send().await?;
        let status = response.status();

        if !status.is_success() {
            return Ok(String::new());
        }

        let content_type = response
            .headers()
            .get("content-type")
            .map_or(String::new(), |ct| ct.to_str().unwrap_or("").to_string());

        if content_type.is_empty() {
            return Ok("".to_string());
        }

        let content = response.text().await?;
        Ok(content)
    }

    async fn get_contents_from_url(&self, url: &str) -> String {
        self.load_contents_from_url(url).await.unwrap_or_default()
    }

    // Statements methods
    async fn get_statements_needing_references(
        &mut self,
        entity: &str,
    ) -> Result<Vec<EntityStatement>> {
        let mut ret = Vec::new();
        let entity = entity.trim().to_uppercase();
        self.entities.load_entity(&self.api, &entity).await?;

        let item = match self.entities.get_entity(&entity) {
            Some(i) => i,
            None => return Ok(ret),
        };

        let claims = item.claims();

        for claim in claims {
            let property = claim.property();
            if self.no_refs_for_properties.contains(property) {
                continue;
            }

            let mainsnak = claim.main_snak();
            let datatype = mainsnak.datatype().to_owned();
            if datatype == SnakDataType::ExternalId || datatype == SnakDataType::CommonsMedia {
                continue; // No refs for external IDs or media
            }

            let statement = EntityStatement {
                entity: entity.clone(),
                property: property.to_string(),
                id: claim.id().unwrap_or_default(),
                claim: claim.clone(),
            };

            ret.push(statement);
        }

        Ok(ret)
    }

    // fn _other_html2text(&self, html: &str) -> String {
    //     let ret = html2text::config::plain_no_decorate()
    //         .string_from_read(html.as_bytes(), usize::MAX)
    //         .unwrap_or_default();
    //     ret
    // }

    fn html2text(&self, html: &str) -> String {
        // TODO use _other_html2text
        let mut ret = html.to_string();

        // Step by step replacements similar to the PHP version
        ret = ret.replace("\n", " ");

        // Remove everything before and including <body>
        if let Some(body_pos) = ret.find("<body") {
            if let Some(close_pos) = ret[body_pos..].find(">") {
                ret = ret[body_pos + close_pos + 1..].to_string();
            }
        }

        // Remove everything after and including </body>
        if let Some(end_body_pos) = ret.find("</body>") {
            ret = ret[0..end_body_pos].to_string();
        }

        // Remove HTML comments
        let comment_regex = Regex::new(r"<!--.*?-->").unwrap();
        ret = comment_regex.replace_all(&ret, " ").to_string();

        // Replace closing tags with newlines
        let p_div_br_regex = Regex::new(r"</(p|div|br)>").unwrap();
        ret = p_div_br_regex.replace_all(&ret, "\n").to_string();

        // Replace self-closing <br> with newlines
        let br_regex = Regex::new(r"<br\s*/>").unwrap();
        ret = br_regex.replace_all(&ret, "\n").to_string();

        // Remove all tags
        let tag_regex = Regex::new(r"<.+?>").unwrap();
        ret = tag_regex.replace_all(&ret, " ").to_string();

        // Normalize whitespace
        let whitespace_regex = Regex::new(r"[\r\t ]+").unwrap();
        ret = whitespace_regex.replace_all(&ret, " ").to_string();

        // Clean up space + newline combinations
        ret = ret.replace(" \n", "\n").replace("\n ", "\n");

        // Collapse multiple newlines
        let newlines_regex = Regex::new(r"\n+").unwrap();
        ret = newlines_regex.replace_all(&ret, "\n").to_string();

        // Collapse multiple spaces
        let spaces_regex = Regex::new(r" +").unwrap();
        ret = spaces_regex.replace_all(&ret, " ").to_string();

        ret
    }

    // fn _new_guess_page_language_from_text(&self, text: &str) -> String {
    //     let detector = lingua::LanguageDetectorBuilder::from_all_languages().build();
    //     let detected_language = detector
    //         .detect_language_of(text)
    //         .map(|l| l.iso_code_639_1().to_string());
    //     let detected_language = detected_language.unwrap_or("en".to_string());
    //     println!("Detected language: {detected_language}");
    //     detected_language
    // }

    fn guess_page_language_from_text(&self, text: &str) -> String {
        // TODO use _new_guess_page_language_from_text
        let mut ret = "en".to_string(); // Default
        let mut candidates = HashMap::new();

        // Count occurrences of common words in different languages
        let en_regex = Regex::new(r"\b(he|she|it|is|was|the|a|an)\b").unwrap();
        candidates.insert("en", en_regex.find_iter(text).count());

        let de_regex = Regex::new(r"\b(er|sie|es|das|ein|eine|war|ist)\b").unwrap();
        candidates.insert("de", de_regex.find_iter(text).count());

        let it_regex = Regex::new(r"\b(è|una|della|la|nel|si|su|una|di)\b").unwrap();
        candidates.insert("it", it_regex.find_iter(text).count());

        let fr_regex = Regex::new(r"\b(est|un|une|et|la|il|a|de|par)\b").unwrap();
        candidates.insert("fr", fr_regex.find_iter(text).count());

        let es_regex = Regex::new(r"\b(el|es|un|de|a|la|es|conlas|dos)\b").unwrap();
        candidates.insert("es", es_regex.find_iter(text).count());

        // Find language with highest count
        let mut best = 5; // Enforce default for incomprehensible text
        for (language, &count) in &candidates {
            if count <= best {
                continue;
            }
            best = count;
            ret = language.to_string();
        }

        ret
    }

    async fn get_candidate_urls_from_wikis(&self, item: &Entity) -> UniqueUrlCandidates {
        let entity = item.id();
        if self.entities.load_entity(&self.api, entity).await.is_err() {
            return HashMap::new();
        }

        let item = match self.entities.get_entity(entity) {
            Some(i) => i,
            None => return HashMap::new(),
        };

        let mut wiki_page_to_load = vec![];
        let sitelinks = item.sitelinks().to_owned().unwrap_or_default();
        for sitelink in sitelinks {
            let page = sitelink.title();
            let wiki = sitelink.site();
            if page.contains(':') {
                continue; // Poor man's namespace detection
            }

            // Load external links for wiki page
            let server = self.get_web_server_for_wiki(wiki);
            let url = format!(
                "https://{}/w/api.php?action=query&prop=extlinks&ellimit=500&elexpandurl=1&format=json&titles={}",
                server,
                page.replace(' ', "_")
            );

            wiki_page_to_load.push((wiki.to_string(), page.to_string(), url.to_string()));
        }

        let mut futures = vec![];
        for (_wiki, _page, url) in &wiki_page_to_load {
            let future = self.load_json_from_url(url);
            futures.push(future);
        }
        // println!("LOADING {} Wiki pages", futures.len());
        let wiki_pages: Vec<serde_json::Value> =
            join_all(futures).await.into_iter().flatten().collect();
        // println!("LOADED  {} Wiki pages", wiki_pages.len());

        let mut futures = vec![];
        for json in &wiki_pages {
            let future = self.generate_url_candidates_for_wiki_page(json);
            futures.push(future);
        }
        let candidate_urls: Vec<String> = join_all(futures).await.into_iter().flatten().collect();

        let mut futures = vec![];
        for url in &candidate_urls {
            let future = self.generate_url_candidate(url);
            futures.push(future);
        }
        // println!("LOADING {} Wiki-based candidates", futures.len());
        let url_candidates: HashMap<String, UrlCandidate> = join_all(futures)
            .await
            .into_iter()
            .flatten()
            .map(|uc| (uc.url.clone(), uc))
            .collect();
        // println!("LOADED  {} Wiki-based candidates", url_candidates.len());

        url_candidates
    }

    async fn load_json_from_url(&self, url: &str) -> Option<Value> {
        let response = self.get_contents_from_url(url).await;
        serde_json::from_str(&response).ok()
    }

    async fn generate_url_candidates_for_wiki_page(&self, json: &Value) -> Vec<String> {
        let mut had_url = HashSet::new();
        let mut candidates = vec![];
        if let Some(query) = json.get("query") {
            if let Some(pages) = query.get("pages") {
                if let Some(pages_obj) = pages.as_object() {
                    for (_, page_info) in pages_obj {
                        if let Some(extlinks) = page_info.get("extlinks") {
                            if let Some(links) = extlinks.as_array() {
                                for link in links {
                                    if let Some(url) = link.get("*").and_then(|u| u.as_str()) {
                                        // Basic validation
                                        if !url.starts_with("http") {
                                            continue;
                                        }

                                        // Skip Wikimedia "sources"
                                        if RE_WIKI.is_match(url) {
                                            continue;
                                        }

                                        if had_url.contains(url) {
                                            continue;
                                        }
                                        had_url.insert(url.to_string());

                                        candidates.push(url.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        candidates
    }

    async fn generate_url_candidate(&self, url: &str) -> Option<UrlCandidate> {
        let contents = self.get_contents_from_url(url).await;
        if contents.is_empty() {
            return None;
        }
        let text = self.html2text(&contents);
        let language = self.guess_page_language_from_text(&text);
        let ret = UrlCandidate {
            url: url.to_string(),
            url_type: UrlType::WikiExternal,
            property: None,
            external_id: None,
            stated_in: None,
            language,
            text,
        };
        Some(ret)
    }

    // Helper method: get web server for wiki
    fn get_web_server_for_wiki(&self, wiki: &str) -> String {
        let parts: Vec<&str> = wiki.split("wik").collect();
        let lang = parts[0];

        if wiki.ends_with("wiki") {
            format!("{}.wikipedia.org", lang)
        } else if wiki.ends_with("wikisource") {
            format!("{}.wikisource.org", lang)
        } else if wiki.ends_with("wiktionary") {
            format!("{}.wiktionary.org", lang)
        } else if wiki.ends_with("wikiquote") {
            format!("{}.wikiquote.org", lang)
        } else {
            format!("{}.wikipedia.org", lang) // Default fallback
        }
    }

    async fn get_candidate_urls(&mut self, entity: &str) -> Result<UniqueUrlCandidates> {
        self.entities.load_entity(&self.api, entity).await?;
        let item = match self.entities.get_entity(entity) {
            Some(i) => i.clone(),
            None => return Ok(HashMap::new()),
        };

        if item.claims().is_empty() {
            return Ok(HashMap::new()); // Has no claims
        }

        let f1 = self.get_candidate_urls_from_wikis(&item);
        let f2 = self.get_direct_websites(&item);
        let f3 = self.get_candidates_for_external_ids(&item);
        let (from_wikis, official_websites, external_ids) = join!(f1, f2, f3);

        let mut ret = from_wikis
            .into_iter()
            .chain(official_websites.into_iter())
            .chain(external_ids.into_iter())
            .collect();

        self.add_stated_in(&mut ret).await?;

        Ok(ret)
    }

    async fn add_stated_in(&self, concise_urls: &mut HashMap<String, UrlCandidate>) -> Result<()> {
        // Ensure all used properties are loaded
        let mut properties: Vec<String> = concise_urls
            .iter()
            .filter_map(|(_k, v)| v.property.to_owned())
            .filter(|p| !self.entities.has_entity(p))
            .collect();
        properties.sort();
        properties.dedup();
        self.entities.load_entities(&self.api, &properties).await?;

        // Add "stated in" where possible
        concise_urls.iter_mut().for_each(|(_k, uc)| {
            if let Some(stated_in) = self.add_stated_in_to_url_candidate(uc) {
                uc.stated_in = Some(stated_in);
            }
        });
        Ok(())
    }

    fn add_stated_in_to_url_candidate(&self, uc: &mut UrlCandidate) -> Option<String> {
        let property = uc.property.as_ref()?;
        let prop = self.entities.get_entity(property)?;
        let claims = prop.claims_with_property("P9073");
        let claim = claims.first()?;
        let dv = claim.main_snak().data_value().as_ref()?;
        match dv.value() {
            wikibase::Value::Entity(ev) => Some(ev.id().to_string()),
            _ => None,
        }
    }

    async fn get_candidates_for_external_ids(&self, item: &Entity) -> UniqueUrlCandidates {
        let mut prop_id = Vec::new();
        let claims = item.claims();
        for claim in claims {
            let mainsnak = claim.main_snak();
            let datatype = mainsnak.datatype().to_owned();
            if datatype != SnakDataType::ExternalId {
                continue; // Skip other than external IDs
            }
            if let Some(datavalue) = mainsnak.data_value() {
                let value = datavalue.value();
                let property = claim.property();
                match value {
                    wikibase::Value::StringValue(external_id) => {
                        prop_id.push((property.to_string(), external_id.to_string()));
                    }
                    _ => continue,
                };
            }
        }
        prop_id.sort();
        prop_id.dedup();

        let mut properties = prop_id
            .iter()
            .map(|(p, _)| p)
            .cloned()
            .collect::<Vec<String>>();
        properties.sort();
        properties.dedup();
        match self.entities.load_entities(&self.api, &properties).await {
            Ok(_) => {}
            Err(_) => return HashMap::new(),
        }

        let mut futures = vec![];
        let mut url_in_use = HashSet::new();
        for (property, external_id) in &prop_id {
            // Get property formatter URL
            let ip = match self.entities.get_entity(property) {
                Some(i) => i,
                None => continue,
            };

            let formatter_urls = self.get_string_values_for_property(&ip, "P1630");
            if formatter_urls.is_empty() {
                continue;
            }

            let url = formatter_urls[0].replace("$1", external_id);

            if url_in_use.contains(&url) {
                continue;
            }
            url_in_use.insert(url.clone());

            let future = self.get_url_candidate_from_external_id(property, external_id, url);
            futures.push(future);
        }
        // println!("LOADING {} futures for ext_ids", futures.len());
        let ret: UniqueUrlCandidates = join_all(futures)
            .await
            .into_iter()
            .flatten()
            .map(|uc| (uc.url.to_string(), uc))
            .collect();
        // println!("LOADED  {} futures for ext_ids", ret.len());

        ret
    }

    async fn get_url_candidate_from_external_id(
        &self,
        property: &str,
        external_id: &str,
        url: String,
    ) -> Option<UrlCandidate> {
        let contents = self.get_contents_from_url(&url).await;
        if contents.is_empty() {
            return None;
        }
        let text = self.html2text(&contents);
        let language = self.guess_page_language_from_text(&text);
        let ret = UrlCandidate {
            url,
            url_type: UrlType::ExternalId,
            property: Some(property.to_string()),
            external_id: Some(external_id.to_string()),
            stated_in: None,
            language,
            text,
        };
        Some(ret)
    }

    fn get_string_values_for_property(&self, item: &Entity, property: &str) -> Vec<String> {
        item.claims_with_property(property)
            .into_iter()
            .filter_map(|s| {
                let mainsnak = s.main_snak();
                let datevalue = mainsnak.data_value().to_owned()?;
                match datevalue.value() {
                    wikibase::Value::StringValue(s) => Some(s.to_owned()),
                    _ => None,
                }
            })
            .collect()
    }
    async fn get_direct_websites(&self, item: &Entity) -> UniqueUrlCandidates {
        let official_websites = self.get_string_values_for_property(item, "P856");
        let described_at_url = self.get_string_values_for_property(item, "P973");
        let mut websites: Vec<_> = official_websites
            .into_iter()
            .chain(described_at_url.into_iter())
            .collect();
        websites.sort();
        websites.dedup();
        let mut futures = vec![];
        for website in &websites {
            let future = self.get_contents_from_url(website);
            futures.push(future);
        }
        // println!("LOADING {} futures for official_websites", futures.len());
        let ret: UniqueUrlCandidates = join_all(futures)
            .await
            .into_iter()
            .zip(websites)
            .filter(|(html, _url)| !html.is_empty())
            .map(|(html, url)| {
                let text = self.html2text(&html);
                let language = self.guess_page_language_from_text(&text);
                (
                    url.to_string(),
                    UrlCandidate {
                        url: url.to_string(),
                        url_type: UrlType::DirectWebsite,
                        property: None,
                        external_id: None,
                        stated_in: None,
                        language,
                        text,
                    },
                )
            })
            .collect();
        // println!("LOADED  {} futures for official_websites", ret.len());
        ret
    }

    async fn get_statement_search_patterns(
        &self,
        statement: &EntityStatement,
        language: &str,
    ) -> Result<Vec<String>> {
        let mut ret = Vec::new();

        if self.no_refs_for_properties.contains(&statement.property) {
            return Ok(ret);
        }

        let mainsnak = statement.claim.main_snak();
        let datavalue = match mainsnak.data_value() {
            Some(dv) => dv,
            None => return Ok(ret),
        };
        let value = datavalue.value();
        let dv_type = datavalue.value_type();

        match dv_type {
            DataValueType::Time => {
                let time_value = match value {
                    wikibase::Value::Time(tv) => tv,
                    _ => return Ok(ret),
                };
                let time_str = time_value.time();

                let re = Regex::new(r"^[+-]{0,1}0*(\d+)-(\d\d)-(\d\d)").unwrap();
                if let Some(caps) = re.captures(time_str) {
                    let year = caps.get(1).map_or("", |m| m.as_str());
                    let month = caps.get(2).map_or("", |m| m.as_str()).to_string();
                    let day = caps.get(3).map_or("", |m| m.as_str()).to_string();
                    let precision = *time_value.precision();

                    if precision == 9 {
                        // Year precision
                        ret.push(year.to_string());
                    } else if precision == 11 {
                        // Day precision
                        let month_num = month.parse::<u32>().unwrap_or(1);
                        let day_num = day.parse::<u32>().unwrap_or(1);
                        let year_num = year.parse::<i32>().unwrap_or(2000);

                        // Format date with Chrono
                        let _date = NaiveDate::from_ymd_opt(year_num, month_num, day_num)
                            .unwrap_or_else(|| NaiveDate::from_ymd_opt(2000, 1, 1).unwrap());

                        // Add different date formats

                        // Add ISO format
                        ret.push(format!("{}-{}-{}", year, month, day));

                        // Add locale-specific formats
                        Self::add_locale_specific_dates(
                            language, &mut ret, year, month_num, day_num,
                        );
                    }
                }
            }
            DataValueType::StringType => {
                if let wikibase::Value::StringValue(string_val) = value {
                    ret.push(string_val.to_string());
                }
            }
            DataValueType::MonoLingualText => {
                if let wikibase::Value::MonoLingual(mono_text) = value {
                    ret.push(mono_text.text().to_string());
                }
            }
            DataValueType::EntityId => {
                if let wikibase::Value::Entity(ev) = value {
                    let entity_id = ev.id();
                    self.entities.load_entity(&self.api, entity_id).await?;
                    let vi = match self.entities.get_entity(entity_id) {
                        Some(i) => i,
                        None => return Ok(ret),
                    };
                    let mut aliases: Vec<String> = vi
                        .aliases()
                        .iter()
                        .filter(|s| s.language() == language)
                        .map(|s| s.value().to_owned())
                        .collect();
                    let label_mul: Option<String> = vi
                        .labels()
                        .iter()
                        .filter(|s| s.language() == "mul")
                        .map(|s| s.value().to_owned())
                        .next();
                    let label_opt: Option<String> = vi
                        .labels()
                        .iter()
                        .filter(|s| s.language() == language)
                        .map(|s| s.value().to_owned())
                        .next();

                    if let Some(label) = label_opt {
                        aliases.insert(0, label); // Make label first entry
                    }
                    if let Some(label) = label_mul {
                        aliases.insert(0, label); // Make label first entry
                    }

                    for alias in aliases {
                        let alias_quoted = regex::escape(alias.trim());
                        if alias_quoted.len() < 3 {
                            continue;
                        }
                        ret.push(alias_quoted);
                    }
                }
            }
            DataValueType::GlobeCoordinate => {
                // Ignore
            }
            DataValueType::Quantity => {
                // Ignore
            }
            _ => {
                // Unknown type
            }
        }

        Ok(ret)
    }

    fn does_statement_have_this_reference(
        &self,
        statement: &EntityStatement,
        url_candidate: &UrlCandidate,
    ) -> bool {
        let claim = &statement.claim;
        let references = claim.references();

        for reference in references {
            let snaks = reference.snaks();

            // Check for reference URL (P854)
            let p854_array = snaks
                .iter()
                .filter(|snak| snak.property() == "P854")
                .collect::<Vec<&Snak>>();
            for snak in p854_array {
                if let Some(wikibase::Value::StringValue(url)) =
                    snak.data_value().as_ref().map(|dv| dv.value())
                {
                    if *url == url_candidate.url {
                        return true;
                    }
                }
            }

            // Check for reference prop=>value for external-id type
            if url_candidate.url_type == UrlType::ExternalId {
                if let Some(ref_prop) = &url_candidate.property {
                    let snaks_array = snaks
                        .iter()
                        .filter(|snak| snak.property() == ref_prop)
                        .collect::<Vec<&Snak>>();

                    for snak in snaks_array {
                        if let Some(wikibase::Value::StringValue(s)) =
                            snak.data_value().as_ref().map(|dv| dv.value())
                        {
                            if let Some(external_id) = &url_candidate.external_id {
                                if s == external_id {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }

        false
    }

    async fn is_supported_entity(&mut self, entity: &str) -> Result<bool> {
        self.entities.load_entity(&self.api, entity).await?;

        let item = match self.entities.get_entity(entity) {
            Some(i) => i,
            None => return Ok(false),
        };

        for (property, target) in &self.unsupported_entity_markers {
            if item.has_target_entity(property, target) {
                return Ok(false);
            }
        }

        Ok(true)
    }

    pub async fn get_potential_references(
        &mut self,
        entity: &str,
    ) -> Result<Vec<ConciseUrlCandidate>> {
        let entity = entity.trim().to_uppercase();

        if !self.is_supported_entity(&entity).await? {
            return Ok(vec![]);
        }

        let statements = self.get_statements_needing_references(&entity).await?;
        if statements.is_empty() {
            return Ok(vec![]);
        }

        let url_candidates = self.get_candidate_urls(&entity).await?;
        if url_candidates.is_empty() {
            return Ok(vec![]);
        }

        let mut futures = vec![];
        for statement in &statements {
            let future = self.process_statement(statement, &url_candidates);
            futures.push(future);
        }
        let mut ret: Vec<ConciseUrlCandidate> = join_all(futures)
            .await
            .into_iter()
            .filter_map(|r| r.ok())
            .flatten()
            .filter(|r| r.property != Some("P973".to_string())) // Remove references for "described at URL"
            .collect();
        ret.sort();
        let ret = Self::merge_cuc_candidates(ret);

        Ok(ret)
    }

    fn merge_cuc_candidates(mut input: Vec<ConciseUrlCandidate>) -> Vec<ConciseUrlCandidate> {
        let mut ret = vec![input.remove(0)];
        for current in input {
            let last = ret.last_mut().unwrap(); //Safe
            if current == *last {
                last.texts.extend(current.texts);
            } else {
                ret.push(current);
            }
        }
        for cuc in &mut ret {
            cuc.texts.sort();
            cuc.texts.dedup();
        }
        ret
    }

    async fn process_statement(
        &self,
        statement: &EntityStatement,
        url_candidates: &HashMap<String, UrlCandidate>,
    ) -> Result<Vec<ConciseUrlCandidate>> {
        let mut ret = Vec::new();
        let statement_id = match statement.claim.id() {
            Some(id) => id.to_owned(),
            None => return Ok(ret),
        };

        for url_candidate in url_candidates.values() {
            if self.does_statement_have_this_reference(statement, url_candidate) {
                continue;
            }

            if statement.property == "P27" && url_candidate.url.contains("www.invaluable.com") {
                continue;
            }

            let patterns = self
                .get_statement_search_patterns(statement, &url_candidate.language)
                .await?;

            for pattern in patterns {
                if pattern.trim().is_empty() {
                    continue;
                }

                let re_pattern = format!(r"\b(.{{0,60}})\b({})\b(.{{0,60}})\b", pattern);
                if let Ok(re) = Regex::new(&re_pattern) {
                    if let Some(caps) = re.captures(&url_candidate.text) {
                        let before = caps.get(1).map_or("", |m| m.as_str()).to_string();
                        let matched = caps.get(2).map_or("", |m| m.as_str()).to_string();
                        let after = caps.get(3).map_or("", |m| m.as_str()).to_string();

                        let tp = TextPart {
                            before,
                            regexp_match: matched,
                            after,
                        };
                        ret.push(ConciseUrlCandidate::new(&statement_id, url_candidate, &tp))
                    }
                }
            }
        }
        Ok(ret)
    }

    // fn get_linked_entity(&self, statement: &EntityStatement) -> Option<String> {
    //     match statement.claim.main_snak().data_value().as_ref()?.value() {
    //         wikibase::Value::Entity(entity_value) => Some(entity_value.id().to_string()),
    //         _ => None,
    //     }
    // }

    fn add_locale_specific_dates(
        language: &str,
        ret: &mut Vec<String>,
        year: &str,
        month_num: u32,
        day_num: u32,
    ) {
        ret.push(format!("{year}-{month_num:02}-{day_num:02}")); // ISO
        match language {
            "en" => {
                let month_names = [
                    "",
                    "January",
                    "February",
                    "March",
                    "April",
                    "May",
                    "June",
                    "July",
                    "August",
                    "September",
                    "October",
                    "November",
                    "December",
                ];

                let long_month = month_names.get(month_num as usize).unwrap_or(&"");
                let short_month = &long_month[0..std::cmp::min(3, long_month.len())];

                ret.push(format!("{long_month} {day_num}, {year}"));
                ret.push(format!("{short_month} {day_num}, {year}"));
            }
            "de" => {
                let month_names = [
                    "",
                    "Januar",
                    "Februar",
                    "März",
                    "April",
                    "Mai",
                    "Juni",
                    "Juli",
                    "August",
                    "September",
                    "Oktober",
                    "November",
                    "Dezember",
                ];

                let long_month = month_names.get(month_num as usize).unwrap_or(&"");
                let short_month = &long_month[0..std::cmp::min(3, long_month.len())];

                ret.push(format!("{day_num}. {long_month} {year}"));
                ret.push(format!("{day_num}. {short_month} {year}"));
                ret.push(format!("{day_num:02}. {long_month} {year}"));
                ret.push(format!("{day_num:02}. {short_month} {year}"));

                ret.push(format!("{day_num}. {month_num}. {year}"));
                ret.push(format!("{day_num}.{month_num}.{year}"));
                ret.push(format!("{day_num:02}. {month_num:02}. {year}"));
                ret.push(format!("{day_num:02}.{month_num:02}.{year}"));
            }
            "fr" => {
                let month_names = [
                    "",
                    "janvier",
                    "février",
                    "mars",
                    "avril",
                    "mai",
                    "juin",
                    "juillet",
                    "août",
                    "septembre",
                    "octobre",
                    "novembre",
                    "décembre",
                ];
                let long_month = month_names.get(month_num as usize).unwrap_or(&"");

                ret.push(format!("{} {} {}", day_num, long_month, year));
            }
            _ => {
                // Generic formats
                ret.push(format!("{day_num}. {month_num}. {year}"));
                ret.push(format!("{day_num}.{month_num}.{year}"));
                ret.push(format!("{day_num}/{month_num}/{year}"));

                ret.push(format!("{day_num:02}. {month_num:02}. {year}"));
                ret.push(format!("{day_num:02}.{month_num:02}.{year}"));
                ret.push(format!("{day_num:02}/{month_num:02}/{year}"));
            }
        }
    }
}
