use crate::TOOLFORGE_DB;
use crate::wikidata::Wikidata;
use axum::http::StatusCode;
use futures::future::join_all;
use mediawiki::Api;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use tokio::sync::RwLock;
use wikibase::{Reference, Snak, Statement};
use wikimisc::mysql_async::{from_row, prelude::Queryable};

// Bit flags describing which Wikidata "name" classes an item is an instance of.
const MALE: u8 = 1; // Q12308941 male given name
const FEMALE: u8 = 2; // Q11879590 female given name
const UNISEX: u8 = 4; // Q3409032 unisex given name
const GIVEN: u8 = 8; // Q202444 (generic) given name
const FAMILY: u8 = 16; // Q101352 family name
/// Any class that makes an item a given name (regardless of gender).
const GIVEN_NAME_MASK: u8 = MALE | FEMALE | UNISEX | GIVEN;

// Wikidata Q-ids used for queries and statement values.
const Q_MALE_NAME: &str = "Q12308941";
const Q_FEMALE_NAME: &str = "Q11879590";
const Q_UNISEX_NAME: &str = "Q3409032";
const Q_GIVEN_NAME: &str = "Q202444";
const Q_FAMILY_NAME: &str = "Q101352";
const Q_MALE_GENDER: &str = "Q6581097";
const Q_FEMALE_GENDER: &str = "Q6581072";

/// A single Wikidata name item matched for a name token.
#[derive(Clone, Debug)]
struct NameHit {
    /// The item Q-id, e.g. `Q564172`.
    qid: String,
    /// Bit set of the name classes (see [`MALE`] etc.) this item belongs to.
    classes: u8,
    /// `true` if the token matched the item's label, `false` if only an alias.
    from_label: bool,
}

/// Maps the gender/family-name class Q-id to its [`NameHit::classes`] bit.
fn class_bit(qid: &str) -> u8 {
    match qid {
        Q_MALE_NAME => MALE,
        Q_FEMALE_NAME => FEMALE,
        Q_UNISEX_NAME => UNISEX,
        Q_GIVEN_NAME => GIVEN,
        Q_FAMILY_NAME => FAMILY,
        _ => 0,
    }
}

/// Process-wide cache mapping a name token (as queried) to its matched items.
/// This avoids re-querying the database for names seen in earlier requests.
type NameHitCache = HashMap<String, Vec<NameHit>>;
static NAME_CACHE: LazyLock<RwLock<NameHitCache>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Nobiliary particles / surname prefixes (German "von", Dutch "van der",
/// French/Spanish "de la", Italian "di"/"della", Arabic "al"/"bin", Celtic
/// "mac"/"o'", etc.). When one of these precedes the final surname word it is
/// part of the family name, not a given name. Matched case-insensitively.
static SURNAME_PARTICLES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // German / Austrian / Swiss
        "von", "vom", "zu", "zum", "zur", "der", "dem", "den", "und",
        // Dutch / Flemish (tussenvoegsel)
        "van", "ter", "ten", "te", "op", "'t", // French
        "de", "du", "des", "la", "le", "les", "d'", // Italian
        "di", "da", "del", "dello", "della", "dei", "degli", "delle", "dal", "dalla", "dalle", "lo",
        "li", "de'", // Spanish / Portuguese / Galician
        "las", "los", "do", "dos", "das", // Arabic / Persian
        "al", "el", "bin", "ben", "ibn", "abu", "abd", "bint", "abdel", "abdul",
        // Hebrew
        "bar", "ha", // Irish / Scottish / Welsh
        "mac", "mc", "o'", "ó", "ní", "nic", "ua", "ap", "ab", // Scandinavian
        "af", "av",
    ]
    .into_iter()
    .collect()
});

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Person;

impl Person {
    pub async fn name_gender(name: &str) -> Result<Vec<Statement>, StatusCode> {
        let tokens: Vec<&str> = name.split_whitespace().collect();
        if tokens.is_empty() {
            return Ok(vec![]); // No name, return empty set
        }
        let (first_names, surname_words) = Self::split_name(&tokens);
        let surname_full = surname_words.join(" ");
        // The final word of the surname, used as a fallback family-name lookup
        // when the full phrase (incl. particle) has no family-name item.
        let surname_core = (*surname_words.last().unwrap_or(&"")).to_string();

        let mut all_tokens: Vec<&str> = first_names.clone();
        all_tokens.push(surname_full.as_str());
        if surname_core != surname_full {
            all_tokens.push(surname_core.as_str());
        }
        let lookup = Self::gather_hits(&all_tokens).await;

        Ok(Self::resolve(
            &first_names,
            &surname_full,
            &surname_core,
            &lookup,
        ))
    }

    /// Split a whitespace-tokenized name into given names and surname words.
    /// The surname is the trailing run that starts at the earliest nobiliary
    /// particle directly preceding the final word, so `Otto von Bismarck`
    /// yields given name `Otto` and surname `von Bismarck`. Particles never
    /// end up among the given names.
    fn split_name<'a>(tokens: &[&'a str]) -> (Vec<&'a str>, Vec<&'a str>) {
        // The last token is always part of the surname.
        let mut start = tokens.len().saturating_sub(1);
        while start > 0 && SURNAME_PARTICLES.contains(tokens[start - 1].to_lowercase().as_str()) {
            start -= 1;
        }
        (tokens[..start].to_vec(), tokens[start..].to_vec())
    }

    /// Look up all the given tokens, returning a map from token to matched name
    /// items. Results are served from (and stored in) the process cache; cache
    /// misses are fetched from the Toolforge DB, falling back to the Wikidata
    /// API when no DB connection is available.
    async fn gather_hits(tokens: &[&str]) -> NameHitCache {
        let mut result: NameHitCache = HashMap::new();
        let mut misses: Vec<String> = vec![];
        {
            let cache = NAME_CACHE.read().await;
            for &token in tokens {
                if let Some(hits) = cache.get(token) {
                    result.insert(token.to_string(), hits.clone());
                } else if !misses.iter().any(|m| m == token) {
                    misses.push(token.to_string());
                }
            }
        }
        if misses.is_empty() {
            return result;
        }

        // Prefer the database; fall back to the API. Only cache results we are
        // confident about, so a transient connectivity failure does not poison
        // the cache with spurious negatives.
        let (fetched, allow_cache) = match Self::db_lookup(&misses).await {
            Ok(map) => (map, true),
            Err(_) => {
                let map = Self::api_lookup(&misses).await;
                let any = map.values().any(|hits| !hits.is_empty());
                (map, any)
            }
        };

        if allow_cache {
            let mut cache = NAME_CACHE.write().await;
            for token in &misses {
                let hits = fetched.get(token).cloned().unwrap_or_default();
                cache.insert(token.clone(), hits.clone());
                result.insert(token.clone(), hits);
            }
        } else {
            for token in &misses {
                result.insert(
                    token.clone(),
                    fetched.get(token).cloned().unwrap_or_default(),
                );
            }
        }
        result
    }

    /// Fetch name items for `tokens` directly from the Toolforge replicas:
    /// one term-store query to find candidate items by label/alias, then one
    /// Wikidata query to determine which name classes those items belong to.
    async fn db_lookup(tokens: &[String]) -> anyhow::Result<NameHitCache> {
        // Step 1: term store — items whose label (type 1) or alias (type 3)
        // exactly equals one of the tokens.
        let placeholders: String = std::iter::repeat_n("?", tokens.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"SELECT `wbx_text`, `wbit_item_id`, `wbtl_type_id`
                FROM `wbt_text`, `wbt_text_in_lang`, `wbt_term_in_lang`, `wbt_item_terms`
                WHERE `wbx_text` IN ({placeholders})
                AND `wbxl_text_id` = `wbx_id`
                AND `wbtl_text_in_lang_id` = `wbxl_id`
                AND `wbit_term_in_lang_id` = `wbtl_id`
                AND `wbtl_type_id` IN (1, 3)"#
        );
        let mut conn = TOOLFORGE_DB.get_connection("termstore").await?;
        let rows: Vec<(Vec<u8>, u64, u32)> = conn
            .exec_iter(sql, tokens.to_vec())
            .await?
            .map_and_drop(from_row::<(Vec<u8>, u64, u32)>)
            .await?;
        drop(conn);
        if rows.is_empty() {
            return Ok(HashMap::new());
        }

        // Step 2: Wikidata — which of the candidate items are an instance of a
        // gender / given-name / family-name class.
        let qids: Vec<String> = {
            let mut set: HashSet<u64> = HashSet::new();
            for (_, item_id, _) in &rows {
                set.insert(*item_id);
            }
            set.into_iter().map(|id| format!("Q{id}")).collect()
        };
        let qid_placeholders: String = std::iter::repeat_n("?", qids.len())
            .collect::<Vec<_>>()
            .join(",");
        let class_sql = format!(
            r#"SELECT `page_title`, `lt_title`
                FROM `page`, `pagelinks`, `linktarget`
                WHERE `page_title` IN ({qid_placeholders})
                AND `page_namespace` = 0
                AND `pl_from` = `page_id`
                AND `pl_target_id` = `lt_id`
                AND `lt_title` IN ('{Q_MALE_NAME}', '{Q_FEMALE_NAME}', '{Q_UNISEX_NAME}', '{Q_GIVEN_NAME}', '{Q_FAMILY_NAME}')"#
        );
        let mut wd_conn = TOOLFORGE_DB.get_connection("wikidata").await?;
        let class_rows: Vec<(String, String)> = wd_conn
            .exec_iter(class_sql, qids)
            .await?
            .map_and_drop(from_row::<(String, String)>)
            .await?;
        drop(wd_conn);

        let mut qid_classes: HashMap<String, u8> = HashMap::new();
        for (qid, class) in class_rows {
            *qid_classes.entry(qid).or_insert(0) |= class_bit(&class);
        }

        // Assemble hits per token, keeping only items that are actually names.
        let mut out: NameHitCache = HashMap::new();
        for (text, item_id, type_id) in rows {
            let qid = format!("Q{item_id}");
            let classes = match qid_classes.get(&qid) {
                Some(classes) => *classes,
                None => continue,
            };
            let token = String::from_utf8_lossy(&text).into_owned();
            let is_label = type_id == 1;
            let entry = out.entry(token).or_default();
            if let Some(hit) = entry.iter_mut().find(|hit| hit.qid == qid) {
                hit.classes |= classes;
                hit.from_label = hit.from_label || is_label;
            } else {
                entry.push(NameHit {
                    qid,
                    classes,
                    from_label: is_label,
                });
            }
        }
        Ok(out)
    }

    /// Fallback lookup via the Wikidata API/SPARQL, used when no DB connection
    /// is available. One search per class per token, run concurrently.
    async fn api_lookup(tokens: &[String]) -> NameHitCache {
        let api = match Wikidata::get_wikidata_api().await {
            Ok(api) => api,
            Err(_) => return HashMap::new(),
        };
        let futures = tokens
            .iter()
            .map(|token| Self::api_lookup_token(&api, token));
        let results = join_all(futures).await;
        tokens.iter().cloned().zip(results).collect()
    }

    async fn api_lookup_token(api: &Api, token: &str) -> Vec<NameHit> {
        let classes = [
            (MALE, Q_MALE_NAME),
            (FEMALE, Q_FEMALE_NAME),
            (UNISEX, Q_UNISEX_NAME),
            (GIVEN, Q_GIVEN_NAME),
            (FAMILY, Q_FAMILY_NAME),
        ];
        let futures = classes.iter().map(|(bit, class)| async move {
            let items = Wikidata::search_names_by_class(api, token, class)
                .await
                .unwrap_or_default();
            (*bit, items)
        });
        let results = join_all(futures).await;
        let mut by_qid: HashMap<String, u8> = HashMap::new();
        for (bit, items) in results {
            for qid in items {
                *by_qid.entry(qid).or_insert(0) |= bit;
            }
        }
        by_qid
            .into_iter()
            .map(|(qid, class_set)| NameHit {
                qid,
                classes: class_set,
                // The API path matches on the item's label.
                from_label: true,
            })
            .collect()
    }

    /// Turn the looked-up name items into statements: family name (P734),
    /// gender (P21), and given names (P735).
    fn resolve(
        first_names: &[&str],
        surname_full: &str,
        surname_core: &str,
        lookup: &NameHitCache,
    ) -> Vec<Statement> {
        let empty: Vec<NameHit> = Vec::new();
        let get = |token: &str| lookup.get(token).unwrap_or(&empty).as_slice();

        // Gender is decided by position: the first given name that yields a
        // confident vote wins. Later (middle) names only get a say when every
        // earlier name was inconclusive. This reflects how cross-gender names
        // are used — "Maria" alone is female, but as a middle name it does not
        // override a leading male name ("Rainer Maria Rilke" is male).
        let resolved_gender = first_names
            .iter()
            .find_map(|token| Self::token_vote(get(token)))
            .map(|is_male| {
                if is_male {
                    Q_MALE_GENDER
                } else {
                    Q_FEMALE_GENDER
                }
            });

        let mut statements = Vec::new();

        // Family name (P734) — only when there is a single unambiguous match.
        // Prefer the full surname phrase including any particle ("van
        // Beethoven"); fall back to the bare final word ("Beethoven").
        let family_qids = {
            let full = Self::family_qids(get(surname_full));
            if full.is_empty() && surname_core != surname_full {
                Self::family_qids(get(surname_core))
            } else {
                full
            }
        };
        if let [qid] = family_qids.as_slice() {
            statements.push(Self::name_statement("P734", qid));
        }

        // Gender (P21).
        if let Some(gender) = resolved_gender {
            statements.push(Self::gender_statement(gender));
        }

        // Given names (P735) — at most one per token, de-duplicated.
        let mut used: HashSet<String> = HashSet::new();
        for token in first_names {
            let pool = Self::class_pool(get(token), GIVEN_NAME_MASK);
            if let Some(hit) = Self::choose_given_name(&pool, resolved_gender) {
                if used.insert(hit.qid.clone()) {
                    statements.push(Self::name_statement("P735", &hit.qid));
                }
            }
        }

        statements
    }

    /// Distinct, sorted family-name (P734) item Q-ids matched for a token.
    fn family_qids(hits: &[NameHit]) -> Vec<String> {
        let mut qids: Vec<String> = Self::class_pool(hits, FAMILY)
            .iter()
            .map(|hit| hit.qid.clone())
            .collect();
        qids.sort_unstable();
        qids.dedup();
        qids
    }

    /// The hits for a token that match `class_mask`, preferring label matches
    /// over alias matches when any label match exists.
    fn class_pool(hits: &[NameHit], class_mask: u8) -> Vec<&NameHit> {
        let matching: Vec<&NameHit> = hits
            .iter()
            .filter(|hit| hit.classes & class_mask != 0)
            .collect();
        let labels: Vec<&NameHit> = matching
            .iter()
            .copied()
            .filter(|hit| hit.from_label)
            .collect();
        if labels.is_empty() { matching } else { labels }
    }

    /// The gender vote for a single given-name token: `Some(true)` male,
    /// `Some(false)` female, `None` if unknown or genuinely either-gender.
    ///
    /// Many names are recorded as a given name for both sexes (e.g. "Maria" is
    /// overwhelmingly female but is also a male middle name). We therefore look
    /// at how many distinct given-name items exist for each sex and require one
    /// to clearly dominate (at least twice as many) before deciding. A name
    /// explicitly tagged as a *unisex* given name always abstains.
    fn token_vote(hits: &[NameHit]) -> Option<bool> {
        let pool = Self::class_pool(hits, GIVEN_NAME_MASK);
        let mut male = 0_usize;
        let mut female = 0_usize;
        for hit in &pool {
            if hit.classes & UNISEX != 0 {
                return None; // explicitly unisex → genuinely either gender
            }
            if hit.classes & MALE != 0 {
                male += 1;
            }
            if hit.classes & FEMALE != 0 {
                female += 1;
            }
        }
        match (male, female) {
            (0, 0) => None,
            (_, 0) => Some(true),
            (0, _) => Some(false),
            (m, f) if m >= 2 * f => Some(true),
            (m, f) if f >= 2 * m => Some(false),
            _ => None, // too close to call
        }
    }

    /// Pick the given-name item to record for a token. When a gender was
    /// resolved, prefer the item of that gender; otherwise only record a name
    /// when it is unambiguous (a single candidate item).
    fn choose_given_name<'a>(pool: &[&'a NameHit], resolved: Option<&str>) -> Option<&'a NameHit> {
        let gender_bit = if resolved == Some(Q_MALE_GENDER) {
            MALE
        } else if resolved == Some(Q_FEMALE_GENDER) {
            FEMALE
        } else {
            0
        };
        if gender_bit != 0 {
            if let Some(hit) = pool
                .iter()
                .copied()
                .filter(|hit| hit.classes & gender_bit != 0)
                .min_by(|a, b| a.qid.cmp(&b.qid))
            {
                return Some(hit);
            }
            // Fall through: the resolved gender has no matching item for this
            // token (e.g. a middle name of the other gender); still record it
            // when there is a single unambiguous candidate.
        }
        let mut qids: Vec<&str> = pool.iter().map(|hit| hit.qid.as_str()).collect();
        qids.sort_unstable();
        qids.dedup();
        if let [qid] = qids.as_slice() {
            pool.iter().copied().find(|hit| hit.qid == *qid)
        } else {
            None
        }
    }

    fn gender_statement(gender: &str) -> Statement {
        let snak = Snak::new_item("P21", gender);
        let reference = Reference::new(vec![
            Wikidata::infernal_reference_snak(),
            Snak::new_item("P3452", "Q69652498"), // inferred from person's given name
        ]);
        Statement::new_normal(snak, vec![], vec![reference])
    }

    /// Build a name statement (`P734` family name or `P735` given name).
    fn name_statement(property: &str, qid: &str) -> Statement {
        let snak = Snak::new_item(property, qid);
        let reference = Reference::new(vec![
            Wikidata::infernal_reference_snak(),
            Snak::new_item("P3452", "Q97033143"), // inferred from person's full name
        ]);
        Statement::new_normal(snak, vec![], vec![reference])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex;

    /// Serializes the live-network tests. They share a small connection pool
    /// (and, locally, an SSH tunnel that drops connections under concurrent
    /// load), so running them one at a time keeps them reliable.
    static NET_TEST_LOCK: Mutex<()> = Mutex::const_new(());

    /// Helper: extract the item value (Q-id) from a statement's main snak
    fn snak_item_value(statement: &Statement) -> Option<String> {
        let dv = statement.main_snak().data_value().as_ref()?;
        if let wikibase::Value::Entity(ev) = dv.value() {
            Some(ev.id().to_string())
        } else {
            None
        }
    }

    fn property_values(statements: &[Statement], property: &str) -> Vec<String> {
        statements
            .iter()
            .filter(|s| s.main_snak().property() == property)
            .filter_map(snak_item_value)
            .collect()
    }

    #[tokio::test]
    async fn test_name_gender_male() {
        let _guard = NET_TEST_LOCK.lock().await;
        // "Heinrich Magnus Manske" — male given names + last name + gender.
        let results = Person::name_gender("Heinrich Magnus Manske").await.unwrap();
        if results.is_empty() {
            return; // No DB and no API connectivity; cannot test offline.
        }
        // Gender should be male.
        assert_eq!(
            property_values(&results, "P21"),
            vec!["Q6581097".to_string()],
            "expected exactly one male gender statement"
        );
        // Given names should be present.
        assert!(
            !property_values(&results, "P735").is_empty(),
            "expected at least one given name statement"
        );
    }

    #[tokio::test]
    async fn test_name_gender_female() {
        let _guard = NET_TEST_LOCK.lock().await;
        // "Elisabeth Manske" — a clearly female first name.
        let results = Person::name_gender("Elisabeth Manske").await.unwrap();
        if results.is_empty() {
            return;
        }
        assert_eq!(
            property_values(&results, "P21"),
            vec!["Q6581072".to_string()],
            "expected exactly one female gender statement"
        );
    }

    fn split(name: &str) -> (Vec<&str>, String) {
        let tokens: Vec<&str> = name.split_whitespace().collect();
        let (first, surname) = Person::split_name(&tokens);
        (first, surname.join(" "))
    }

    #[test]
    fn test_split_name_plain() {
        assert_eq!(
            split("Heinrich Magnus Manske"),
            (vec!["Heinrich", "Magnus"], "Manske".to_string())
        );
    }

    #[test]
    fn test_split_name_single_word() {
        assert_eq!(split("Manske"), (Vec::<&str>::new(), "Manske".to_string()));
    }

    #[test]
    fn test_split_name_particles() {
        // Particles must join the surname and never appear among given names.
        assert_eq!(
            split("Otto von Bismarck"),
            (vec!["Otto"], "von Bismarck".to_string())
        );
        assert_eq!(
            split("Ludwig van Beethoven"),
            (vec!["Ludwig"], "van Beethoven".to_string())
        );
        assert_eq!(
            split("Charles de Gaulle"),
            (vec!["Charles"], "de Gaulle".to_string())
        );
        assert_eq!(
            split("Leonardo da Vinci"),
            (vec!["Leonardo"], "da Vinci".to_string())
        );
    }

    #[test]
    fn test_split_name_multi_word_particles() {
        assert_eq!(
            split("Ursula von der Leyen"),
            (vec!["Ursula"], "von der Leyen".to_string())
        );
        assert_eq!(
            split("Stephanie von und zu Guttenberg"),
            (vec!["Stephanie"], "von und zu Guttenberg".to_string())
        );
    }

    #[test]
    fn test_split_name_case_insensitive_and_no_given_name() {
        // Mixed-case particle still recognised.
        assert_eq!(
            split("Otto Von Bismarck"),
            (vec!["Otto"], "Von Bismarck".to_string())
        );
        // Leading particle with no given name.
        assert_eq!(
            split("von Bismarck"),
            (Vec::<&str>::new(), "von Bismarck".to_string())
        );
    }

    #[tokio::test]
    async fn test_name_gender_particle_surname() {
        let _guard = NET_TEST_LOCK.lock().await;
        // "Ludwig van Beethoven" — male, and "van" must not be a given name.
        let results = Person::name_gender("Ludwig van Beethoven").await.unwrap();
        if results.is_empty() {
            return;
        }
        assert_eq!(
            property_values(&results, "P21"),
            vec!["Q6581097".to_string()],
            "expected male gender"
        );
        // No given-name statement should be the particle's item; at most one
        // given name ("Ludwig") is expected.
        assert!(
            property_values(&results, "P735").len() <= 1,
            "particle should not produce extra given-name statements"
        );
    }

    #[tokio::test]
    async fn test_name_gender_cross_gender_middle_name() {
        let _guard = NET_TEST_LOCK.lock().await;
        // "Rainer Maria Rilke" — "Maria" is also a male given name, but as a
        // middle name it must not neutralize the leading male name "Rainer".
        let results = Person::name_gender("Rainer Maria Rilke").await.unwrap();
        if results.is_empty() {
            return;
        }
        assert_eq!(
            property_values(&results, "P21"),
            vec!["Q6581097".to_string()],
            "leading male given name should decide the gender"
        );
    }

    #[tokio::test]
    async fn test_name_gender_empty() {
        // Empty string: no name parts at all.
        let results = Person::name_gender("").await.unwrap();
        assert!(
            results.is_empty(),
            "empty name should produce no statements"
        );
    }

    #[tokio::test]
    async fn test_name_gender_single_word() {
        let _guard = NET_TEST_LOCK.lock().await;
        // Single word is treated as last name only, no first names → no gender.
        let results = Person::name_gender("Manske").await.unwrap();
        assert!(
            property_values(&results, "P21").is_empty(),
            "single-word name should not produce a gender statement"
        );
    }

    #[tokio::test]
    async fn test_name_gender_references() {
        let _guard = NET_TEST_LOCK.lock().await;
        // Every statement must carry a reference containing the infernal snak (P887).
        let results = Person::name_gender("Heinrich Manske").await.unwrap();
        if results.is_empty() {
            return;
        }
        for statement in &results {
            let refs = statement.references();
            assert!(!refs.is_empty(), "every statement should have a reference");
            let has_infernal = refs
                .iter()
                .any(|r| r.snaks().iter().any(|sn| sn.property() == "P887"));
            assert!(has_infernal, "every reference should contain P887");
        }
    }

    #[tokio::test]
    async fn test_name_gender_consistent_calls() {
        let _guard = NET_TEST_LOCK.lock().await;
        // Calling twice with the same input should yield the same result.
        let r1 = Person::name_gender("Heinrich Manske").await.unwrap();
        let r2 = Person::name_gender("Heinrich Manske").await.unwrap();
        assert_eq!(r1.len(), r2.len(), "repeated calls should match");
        for (a, b) in r1.iter().zip(r2.iter()) {
            assert_eq!(a.main_snak().property(), b.main_snak().property());
            assert_eq!(snak_item_value(a), snak_item_value(b));
        }
    }
}
