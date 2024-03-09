use std::time::{SystemTime, UNIX_EPOCH};
use actix_web::web::Data;
use deadpool_postgres::Pool;
use tokio_postgres::Error;
use tracing::{debug, info};
use crate::models::*;
use crate::utils;

pub async fn get_name_by_namehash(pool: &Pool, name_hash: &str) -> Result<NFTWithPrimary, Error> {
    let client = pool.get().await.unwrap();
    let query = "select an.id, an.name_hash, an.full_name, an.resolver, ap.id, dc.amount
            FROM ans3.ans_name AS an
            LEFT JOIN ans3.ans_primary_name as ap ON an.name_hash = ap.name_hash
            LEFT JOIN ans3.domain_credits as dc ON an.transfer_key = dc.transfer_key
            WHERE an.name_hash = $1 limit 1";

    debug!("get_name_by_namehash query db: {} params {}", &query, name_hash);
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[&name_hash]).await?;

    let primary_id: Option<i64> = row.get(4);
    let is_primary_name = match primary_id {
        Some(pid) => pid > 0,
        None => false,
    };

    let resolver: Option<String> = row.get(3);
    let resolver = resolver.unwrap_or_else(|| "".to_string());

    let balance: Option<i64> = row.get(5);
    let balance = balance.unwrap_or_else(|| 0i64);
    
    let nft = NFTWithPrimary {
        name_hash: row.get(1),
        address: "".to_string(),
        name: row.get(2),
        is_primary_name,
        resolver,
        balance,
    };

    Ok(nft)
}

pub async fn get_names_by_addr(pool: &Pool, address: &str) -> Result<Vec<NFTWithPrimary>, Error> {
    let client = pool.get().await.unwrap();
    let query = "select ao.id, ao.name_hash, ao.address, an.full_name, ap.id, an.resolver, dc.amount
            FROM ans3.ans_nft_owner as ao
            JOIN ans3.ans_name as an ON ao.name_hash = an.name_hash
            LEFT JOIN ans3.ans_primary_name as ap ON ao.name_hash = ap.name_hash
            LEFT JOIN ans3.domain_credits as dc ON an.transfer_key = dc.transfer_key
            where ao.address = $1";
    
    debug!("get_names_by_addr query db: {} params {}", query, &address);
    let query = client.prepare(&query).await.unwrap();
    let _rows = client.query(&query, &[&address]).await?;

    let mut nft_list = Vec::new();

    for row in _rows {
        let primary_id: Option<i64> = row.get(4);
        let is_primary = match primary_id {
            Some(pid) => pid > 0,
            None => false,
        };

        let resolver: Option<String> = row.get(5);
        let resolver = resolver.unwrap_or_else(|| "".to_string());

        let balance: Option<i64> = row.get(6);
        let balance = balance.unwrap_or_else(|| 0i64);
        
        let nft = NFTWithPrimary {
            name_hash: row.get(1),
            address: row.get(2),
            name: row.get(3),
            is_primary_name: is_primary,
            resolver,
            balance,
        };
        nft_list.push(nft);
    }

    Ok(nft_list)
}

pub async fn get_resolvers_by_namehash(pool: &Pool, name_hash: &str) -> Result<Vec<Resolver>, Error> {
    let client = pool.get().await.unwrap();

    let query = "select ar.id, ar.category, ar.version, ar.name from ans3.ans_resolver As ar
        LEFT JOIN ans3.ans_name_version as av ON ar.name_hash = av.name_hash
        WHERE (ar.version=av.version or av.version is null) and ar.name_hash = $1";
    debug!("get_resolvers_by_namehash query db: {} params {}", &query, &name_hash);
    let query = client.prepare(&query).await.unwrap();
    let _rows = client.query(&query, &[&name_hash]).await?;

    let mut resolver_list = Vec::new();

    for row in _rows {
        let r = Resolver {
            name_hash: name_hash.to_string(),
            category: row.get(1),
            content: row.get(3),
            version: row.get(2),
        };
        resolver_list.push(r);
    }

    Ok(resolver_list)
}

pub async fn get_resolver(pool: &Pool, name_hash: &str, category: &str) -> Result<Resolver, Error> {
    let client = pool.get().await.unwrap();

    let query = "select ar.id, ar.category, ar.version, ar.name from ans3.ans_resolver As ar
        LEFT JOIN ans3.ans_name_version as av ON ar.name_hash = av.name_hash
        WHERE (ar.version=av.version or av.version is null) and ar.name_hash = $1 and ar.category = $2 limit 1";
    debug!("get_resolvers_by_namehash query db: {} name_hash {}, category {}", &query, &name_hash, &category);
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[&name_hash, &category]).await?;
    
    let resolver = Resolver {
        name_hash: name_hash.to_string(),
        category: row.get(1),
        content: row.get(3),
        version: row.get(2),
    };

    Ok(resolver)
}

pub async fn get_subdomains_by_namehash(pool: &Pool, name_hash: &str) -> Result<Vec<NFT>, Error> {
    // let client = connect().await?;
    let client = pool.get().await.unwrap();

    let query = "select an.id, an.name_hash, an.name, ao.address from ans3.ans_name as an
            LEFT JOIN ans3.ans_nft_owner as ao ON an.name_hash = ao.name_hash
                WHERE parent = $1";
    debug!("get_subdomains_by_namehash query db: {} param {}", &query, &name_hash);
    let query = client.prepare(&query).await.unwrap();
    let _rows = client.query(&query, &[&name_hash]).await?;

    let mut subdomains = Vec::new();

    for row in _rows {
        let address: Option<String> = row.get(3);
        let address = address.unwrap_or_else(|| "".to_string());

        let r = NFT {
            name_hash: row.get(1),
            name: row.get(2),
            address,
        };
        subdomains.push(r);
    }

    Ok(subdomains)
}

pub async fn get_primary_name_by_address(pool: &Data<Pool>, address: &String) -> Result<String, Error> {
    let client = pool.get().await.unwrap();

    let query = "select ap.id, an.full_name from ans3.ans_primary_name As ap
        LEFT JOIN ans3.ans_name as an ON ap.name_hash = an.name_hash
        WHERE ap.address = $1 limit 1";

    debug!("get_primary_name_by_address query db: {} address {}", &query, &address);
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[&address]).await?;
    Ok(row.get(1))
}

pub async fn get_hash_by_name(pool: &Data<Pool>, name: &String) -> Result<String, Error> {
    let client = pool.get().await.unwrap();
    let query = "select id, name_hash from ans3.ans_name WHERE full_name = $1 limit 1";
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[&name]).await?;
    Ok(row.get(1))
}

pub async fn get_address_by_hash(pool: &Data<Pool>, name_hash: &String) -> Result<String, Error> {
    let client = pool.get().await.unwrap();
    let query = "select id, address from ans3.ans_nft_owner WHERE name_hash = $1 limit 1";
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[&name_hash]).await?;
    Ok(row.get(1))
}

pub(crate) async fn get_statistic_data(pool: &Pool) -> Result<AnsStatistic, Error>{
    let client = pool.get().await.unwrap();
    let cur_time = utils::get_current_timestamp();
    let time_24_before:i64 = (cur_time - 24 * 3600) as i64;

    let query = "SELECT COUNT(*) AS total_count,
        COUNT(CASE WHEN ab.timestamp > $1 THEN 1 END) AS count_gt_time
        FROM ans3.ans_name an left JOIN ans3.block ab ON an.block_height = ab.height";
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[&time_24_before]).await?;
    let total_names = row.get(0);
    let total_names_24h = row.get(1);

    let query2 = "SELECT COUNT(*) AS total_count, COUNT(distinct address) AS address_count
        FROM ans3.ans_nft_owner";
    let query2 = client.prepare(&query2).await.unwrap();
    let row2 = client.query_one(&query2, &[]).await?;
    let total_nft:i64 = row2.get(0);
    let total_owner = row2.get(1);

    let query_last_block = "SELECT height FROM ans3.block order by height desc limit 1";
    let query_last_block = client.prepare(&query_last_block).await.unwrap();
    let row_last_block = client.query_one(&query_last_block, &[]).await?;
    let block_height:i64 = row_last_block.get(0);

    let st= AnsStatistic {
        healthy: true,
        block_height,
        cal_time: cur_time,
        total_names,
        total_names_24h,
        total_pri_names: total_names - total_nft,
        total_nft_owners: total_owner
    };
    Ok(st)
}

pub(crate) async fn is_n_query_from_api(pool: &Pool) -> bool {
    let indexer_height = query_last_block_height(pool).await;

    let last_height: u32 = match get_kv_value(pool, "api_height").await {
        Ok(v) => v.parse().unwrap(),
        Err(_) => 0u32
    };

    info!("is_n_query_from_api : {} last_height {}", indexer_height, last_height);
    return last_height - indexer_height > 16;
}

async fn query_last_block_height(pool: &Pool) -> u32 {
    let client = pool.get().await.unwrap();

    let mut indexer_height = 0u32;
    let query = "select height from ans3.block order by height desc limit 1";
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[]).await;
    match row {
        Ok(row) => {
            if !row.is_empty() {
                indexer_height = row.get(0);
                info!("set indexer:api_height: {}", indexer_height);
            }
        }
        Err(_) => {}
    }
    indexer_height
}

pub(crate) async fn get_kv_value(pool: &Pool, key: &str) -> Result<String, Error> {
    let client = pool.get().await.unwrap();

    let query = "select value from ans3.kv where key=$1 limit 1";
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[&key]).await?;
    Ok(row.get(0))
}

pub(crate) async fn set_kv_value(pool: &Pool, key: &str, value: &str) {
    let client = pool.get().await.unwrap();

    let current_time = SystemTime::now();
    let timestamp = current_time.duration_since(UNIX_EPOCH).expect("Failed to get timestamp");
    let current_ts = timestamp.as_secs() as i64;

    client.execute("INSERT INTO ans3.kv (key, value) \
                             VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = $2, updated = $3",
                     &[&key, &value, &current_ts]
    ).await.unwrap();
}