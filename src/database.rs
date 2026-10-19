//! Connections to the Wikidata replica and its term store.
//!
//! The `toolforge` crate finds the servers and the credentials: from the
//! `TOOL_REPLICA_*` environment variables, or from `replica.my.cnf` in
//! `$TOOL_DATA_DIR` or the home directory. For local development, add
//! `local='true'` and a `[tunnels]` section to a copy of that file:
//!
//! ```ini
//! [tunnels]
//! wikidatawiki = 3309
//! termstore.wikidatawiki = 3317
//! ```
//!
//! and open the tunnels with `toolforge-tunnel`; `run_live_tests.sh` does
//! both for the tests that need the databases.

use anyhow::{Result, anyhow};
use std::sync::LazyLock;
use std::time::Duration;
use wikimisc::mysql_async::Conn;
use wikimisc::toolforge::db::{
    Cluster, DBConnectionInfo, connection_info, connection_info_for_tables,
};
use wikimisc::toolforge::pool::CustomPool;

const WIKIDATA: &str = "wikidatawiki";

/// Per pool, so both together stay within a tool's limit of 10 connections.
const POOL_MAX: usize = 4;

/// An unreachable server fails a request after this, rather than hang it.
/// Includes waiting for a free connection, e.g. while `InitialSearch` runs
/// its queries in parallel, so it is generous.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// A failure to set up the pools, e.g. without credentials, is kept as an
/// error, so it fails every connection attempt instead of panicking.
static POOLS: LazyLock<Result<Pools>> = LazyLock::new(Pools::new);

#[derive(Debug)]
struct Pools {
    wikidata: CustomPool,
    termstore: CustomPool,
}

impl Pools {
    fn new() -> Result<Self> {
        Ok(Self {
            wikidata: pool(connection_info(WIKIDATA, Cluster::Web)?)?,
            // The term store queries can be slow, e.g. `RLIKE` in
            // `InitialSearch`, so they get the analytics cluster's longer
            // query timeout.
            termstore: pool(connection_info_for_tables(
                WIKIDATA,
                Cluster::Analytics,
                &["wbt_text"],
            )?)?,
        })
    }
}

/// A connection to the Wikidata replica.
pub async fn wikidata() -> Result<Conn> {
    Ok(pools()?.wikidata.connect().await?)
}

/// A connection to Wikidata's term store (the `wbt_*` tables).
pub async fn termstore() -> Result<Conn> {
    Ok(pools()?.termstore.connect().await?)
}

/// Fails if the database credentials can't be found or read.
pub fn check() -> Result<()> {
    pools().map(drop)
}

fn pools() -> Result<&'static Pools> {
    POOLS
        .as_ref()
        .map_err(|e| anyhow!("databases unavailable: {e:#}"))
}

fn pool(info: DBConnectionInfo) -> Result<CustomPool> {
    Ok(CustomPool::builder(info.host, info.user, info.password)
        .port(info.port)
        .database(info.database)
        .pool_max(POOL_MAX)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()?)
}

/// Runs a test that uses the databases. The pools are shared by the whole
/// process, but `#[tokio::test]` gives each test its own runtime, and the
/// first one to end would take the pools' background tasks with it. So all
/// such tests run on this one runtime instead.
#[cfg(test)]
pub fn block_on<F: Future>(future: F) -> F::Output {
    static RUNTIME: LazyLock<tokio::runtime::Runtime> =
        LazyLock::new(|| tokio::runtime::Runtime::new().unwrap());
    RUNTIME.block_on(future)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wikimisc::mysql_async::prelude::Queryable;

    /// Each pool reaches a server with its tables: `wbt_*` only exist in the
    /// term store, and `page` there would be missing or frozen.
    #[test]
    #[ignore = "needs the databases, see run_live_tests.sh"]
    fn test_pools_reach_their_servers() {
        block_on(async {
            let mut wd = wikidata().await.unwrap();
            let page: Option<u64> = wd
                .query_first("SELECT page_id FROM page WHERE page_namespace=0 AND page_title='Q42'")
                .await
                .unwrap();
            assert_eq!(page, Some(138));

            let mut terms = termstore().await.unwrap();
            let label: Option<u64> = terms
                .query_first("SELECT wby_id FROM wbt_type WHERE wby_name='label'")
                .await
                .unwrap();
            assert_eq!(label, Some(1));
        });
    }
}
