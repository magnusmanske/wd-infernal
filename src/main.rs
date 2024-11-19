use axum::{
    extract::Path,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use futures::future::join_all;
use mediawiki::{hashmap, Api};
use std::{collections::HashMap, net::SocketAddr};
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use wikibase::{Snak, Statement};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().len() > 1 {
        let ret = name_gender("Heinrich Magnus Manske").await.unwrap();
        println!("{ret:?}");
        Ok(())
    } else {
        start_server().await
    }
}

async fn start_server() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let cors = CorsLayer::new().allow_origin(Any);

    let app = Router::new()
        .route("/", get(root))
        .route("/P131/:latitude/:longitude", get(p131_wrapper))
        .route("/name_gender/:name", get(name_gender_wrapper))
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new())
        .layer(cors);

    let port: u16 = match std::env::var("WD_INFERNAL_PORT") {
        Ok(port) => port.as_str().parse::<u16>().unwrap_or(8000),
        Err(_) => 8000,
    };

    let address = [0, 0, 0, 0]; // TODOO env::var("AC2WD_ADDRESS")

    let addr = SocketAddr::from((address, port));
    tracing::debug!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Could not create listener");
    axum::serve(listener, app)
        .await
        .expect("Could not start server");
    Ok(())
}

async fn root() -> impl IntoResponse {
    let ret = include_str!("../static/root.html");
    Html(ret)
}

fn wd_infernal_qualifier() -> Snak {
    Snak::new_item("P887", "Q131287902") // based on heuristic: Wikidata Infernal
}

async fn get_wikidata_api() -> Result<Api, StatusCode> {
    match Api::new("https://www.wikidata.org/w/api.php").await {
        Ok(api) => Ok(api),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn name_gender_wrapper(Path(name): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    let statements = name_gender(&name).await?;
    Ok(Json(statements))
}

async fn _search_items(api: &Api, query: &str) -> Result<Vec<String>, StatusCode> {
    let params: HashMap<String, String> =
        hashmap!["action"=>"query","list"=>"search","srnamespace"=>"0","srsearch"=>&query]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    let results = match api.get_query_api_json(&params).await {
        Ok(v) => v,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let results = match results["query"]["search"].as_array() {
        Some(v) => v,
        None => return Ok(vec![]),
    };
    let results: Vec<String> = results
        .iter()
        .map(|result| result["title"].as_str().unwrap().to_owned())
        .collect();
    Ok(results)
}

// Searches Wikidata via the API
async fn search_single_name(api: &Api, name: &str, p31: &str) -> Result<Vec<String>, StatusCode> {
    let query = format!("{name} haswbstatement:P31={p31}");
    let params: HashMap<String, String> =
        hashmap!["action"=>"query","list"=>"search","srnamespace"=>"0","srsearch"=>&query]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    let results = match api.get_query_api_json(&params).await {
        Ok(v) => v,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let results = match results["query"]["search"].as_array() {
        Some(v) => v,
        None => return Ok(vec![]),
    };
    let results: Vec<String> = results
        .iter()
        .map(|result| result["title"].as_str().unwrap().to_owned())
        .collect();
    if results.is_empty() {
        return Ok(results);
    }
    let values = results.join(" wd:");

    let sparql = format!(
        r#"SELECT DISTINCT ?q {{
      VALUES ?q {{ wd:{values} }}
      ?q wdt:P31 wd:{p31} ; rdfs:label ?label . FILTER ( str(?label)="{name}" )
      }}"#
    );

    let json = match api.sparql_query(&sparql).await {
        Ok(json) => json,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let mut items = api.entities_from_sparql_result(&json, "q");
    items.sort();
    items.dedup();

    // If there are multiple items, return none
    if items.len() > 1 {
        items.clear();
    }
    Ok(items)
}

async fn name_gender(name: &str) -> Result<Vec<Statement>, StatusCode> {
    let mut statements = vec![];
    let mut parts = name.split_whitespace().collect::<Vec<_>>();
    let last_name = match parts.pop() {
        Some(name) => name,
        None => return Ok(statements), // No name, return empty set
    };
    let first_names = parts;
    let api = get_wikidata_api().await?;
    add_last_name(last_name, &api, &mut statements).await?;
    add_first_names_gender(first_names, &api, &mut statements).await?;
    Ok(statements)
}

async fn get_given_names_for_gender(
    first_names: &[&str],
    api: &Api,
    gender: &str,
) -> Result<Vec<String>, StatusCode> {
    let futures: Vec<_> = first_names
        .iter()
        .map(|first_name| search_single_name(api, first_name, gender))
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
    let qualifiers = vec![
        wd_infernal_qualifier(),
        Snak::new_item("P3452", "Q69652498"), // inferred from person's given name
    ];
    Statement::new_normal(snak, qualifiers, vec![])
}

async fn add_first_names_gender(
    first_names: Vec<&str>,
    api: &Api,
    statements: &mut Vec<Statement>,
) -> Result<(), StatusCode> {
    let mut results = join_all([
        get_given_names_for_gender(&first_names, api, "Q12308941"), // Male given name
        get_given_names_for_gender(&first_names, api, "Q11879590"), // Female given name
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
        (true, false) => statements.push(gender_statement("Q6581097")), // male
        (false, true) => statements.push(gender_statement("Q6581072")), // female
        _ => {
            // Ignore
        }
    }
    if is_male != is_female {
        // Either male or female
        let name_statements: Vec<_> = male
            .iter()
            .chain(female.iter())
            .map(|q| {
                let snak = Snak::new_item("P735", q);
                let qualifiers = vec![
                    wd_infernal_qualifier(),
                    Snak::new_item("P3452", "Q97033143"), // inferred from person's full name
                ];
                Statement::new_normal(snak, qualifiers, vec![])
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
    let results = search_single_name(api, last_name, "Q101352").await?;
    if results.len() == 1 {
        if let Some(entity) = results.first() {
            let snak = Snak::new_item("P734", entity);
            let qualifiers = vec![
                wd_infernal_qualifier(),
                Snak::new_item("P3452", "Q97033143"), // inferred from person's full name
            ];
            let statement = Statement::new_normal(snak, qualifiers, vec![]);
            statements.push(statement);
        }
    }
    Ok(())
}

async fn p131_wrapper(
    Path((latitude, longitude)): Path<(f64, f64)>,
) -> Result<impl IntoResponse, StatusCode> {
    let statements = p131(latitude, longitude).await?;
    Ok(Json(statements))
}

async fn p131(latitude: f64, longitude: f64) -> Result<Vec<Statement>, StatusCode> {
    // TODO try list=geosearch?
    let radius_km = 1;
    let sparql = format!(
        r#"SELECT ?p131 {{
	        ?q wdt:P625 ?loc ; wdt:P131 ?p131 .

	        SERVICE wikibase:around {{
	          ?q wdt:P625 ?coords .
	          bd:serviceParam wikibase:center "Point({longitude} {latitude})"^^geo:wktLiteral .
	          bd:serviceParam wikibase:radius "{radius_km}" .
	          bd:serviceParam wikibase:distance ?distance
	        }}

	      SERVICE wikibase:label {{
	        bd:serviceParam wikibase:language "en" .
	      }}
	    }}
	    ORDER BY DESC(?distance)
	    LIMIT 5"#
    );
    let api = get_wikidata_api().await?;
    let json = match api.sparql_query(&sparql).await {
        Ok(json) => json,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let mut entities = api.entities_from_sparql_result(&json, "p131");
    entities.sort();
    entities.dedup();
    let statements: Vec<_> = entities
        .iter()
        .map(|entity| {
            let snak = Snak::new_item("P131", entity);
            let qualifiers = vec![
                wd_infernal_qualifier(),
                Snak::new_item("P3452", "Q96623327"), // inferred from coordinate location
            ];
            Statement::new_normal(snak, qualifiers, vec![])
        })
        .collect();
    Ok(statements)
}

#[cfg(test)]
mod tests {
    use wikibase::{EntityType, EntityValue};

    use super::*;

    #[tokio::test]
    async fn test_p131() {
        let latitude = 52.19422713089248;
        let longitude = 0.13009437319916947;
        let result = p131(latitude, longitude).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0]
                .main_snak()
                .data_value()
                .as_ref()
                .unwrap()
                .value()
                .to_owned(),
            wikibase::Value::Entity(EntityValue::new(EntityType::Item, "Q21713103"))
        );
    }

    #[tokio::test]
    async fn test_search_single_name() {
        let api = Api::new("https://www.wikidata.org/w/api.php")
            .await
            .unwrap();
        let results = search_single_name(&api, "Manske", "Q101352").await.unwrap();
        assert_eq!(results, vec!["Q1891133"]);
    }

    #[tokio::test]
    async fn test_name_gender() {
        let results = name_gender("Heinrich Magnus Manske").await.unwrap();
        assert_eq!(results.len(), 4);
    }
}
