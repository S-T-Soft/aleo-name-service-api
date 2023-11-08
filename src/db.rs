use actix_web::web::Data;
use deadpool_postgres::Pool;
use tokio_postgres::Error;
use crate::models::*;

pub async fn get_name_by_namehash(pool: &Pool, name_hash: &str) -> Result<NFTWithPrimary, Error> {
    let client = pool.get().await.unwrap();
    let query = "select an.id, an.name_hash, an.full_name, an.resolver, ap.id from ans.ans_name AS an 
            LEFT JOIN ans.ans_primary_name as ap ON an.name_hash = ap.name_hash
            WHERE an.name_hash = $1 limit 1";
    
    println!("get_name_by_namehash query db: {} params {}", &query, name_hash);
    let row = client.query_one(query, &[&name_hash]).await?;

    let primary_id: Option<i64> = row.get(4);
    let is_primary_name = match primary_id {
        Some(pid) => pid > 0,
        None => false,
    };

    let resolver: Option<String> = row.get(3);
    let resolver = match resolver {
        Some(resolver) => resolver,
        None => "".to_string(),
    };
    
    let nft = NFTWithPrimary {
        name_hash: row.get(1),
        address: "".to_string(),
        name: row.get(2),
        is_primary_name,
        resolver,
    };

    Ok(nft)
}

pub async fn get_names_by_addr(pool: &Pool, address: &str) -> Result<Vec<NFTWithPrimary>, Error> {
    let client = pool.get().await.unwrap();
    let query = "select ao.id, ao.name_hash, ao.address, an.full_name, ap.id, an.resolver from ans.ans_nft_owner as ao 
            JOIN ans.ans_name as an ON ao.name_hash = an.name_hash
            LEFT JOIN ans.ans_primary_name as ap ON ao.name_hash = ap.name_hash
            where ao.address = $1";
    
    println!("get_names_by_addr query db: {} params {}", query, &address);
    let _rows = client.query(query, &[&address]).await?;

    let mut nft_list = Vec::new();

    for row in _rows {
        let primary_id: Option<i64> = row.get(4);
        let is_primary = match primary_id {
            Some(pid) => pid > 0,
            None => false,
        };

        let resolver: Option<String> = row.get(5);
        let resolver = match resolver {
            Some(resolver) => resolver,
            None => "".to_string(),
        };
        
        let nft = NFTWithPrimary {
            name_hash: row.get(1),
            address: row.get(2),
            name: row.get(3),
            is_primary_name: is_primary,
            resolver,
        };
        nft_list.push(nft);
    }

    Ok(nft_list)
}

pub async fn get_resolvers_by_namehash(pool: &Pool, name_hash: &str) -> Result<Vec<Resolver>, Error> {
    let client = pool.get().await.unwrap();

    let query = "select ar.id, ar.category, ar.version, ar.name from ans.ans_resolver As ar
        LEFT JOIN ans.ans_name_version as av ON ar.name_hash = av.name_hash
        WHERE ar.version=av.version and ar.name_hash = $1";
    println!("get_resolvers_by_namehash query db: {} params {}", &query, &name_hash);

    let _rows = client.query(query, &[&name_hash]).await?;

    let mut resolver_list = Vec::new();

    for row in _rows {
        let r = Resolver {
            name_hash: name_hash.to_string(),
            category: row.get(1),
            name: row.get(3),
            version: row.get(2),
        };
        resolver_list.push(r);
    }

    Ok(resolver_list)
}

pub async fn get_resolver(pool: &Pool, name_hash: &str, category: &str) -> Result<Resolver, Error> {
    let client = pool.get().await.unwrap();

    let query = "select ar.id, ar.category, ar.version, ar.name from ans.ans_resolver As ar
        LEFT JOIN ans.ans_name_version as av ON ar.name_hash = av.name_hash
        WHERE ar.version=av.version and ar.name_hash = $1 and ar.category = $2 limit 1";
    println!("get_resolvers_by_namehash query db: {} name_hash {}, category {}", &query, &name_hash, &category);

    let row = client.query_one(query, &[&name_hash, &category]).await?;
    
    let resolver = Resolver {
        name_hash: name_hash.to_string(),
        category: row.get(1),
        name: row.get(3),
        version: row.get(2),
    };

    Ok(resolver)
}

pub async fn get_subdomains_by_namehash(pool: &Pool, name_hash: &str) -> Result<Vec<NFT>, Error> {
    // let client = connect().await?;
    let client = pool.get().await.unwrap();

    let query = "select an.id, an.name_hash, an.name, ao.address from ans.ans_name as an
            LEFT JOIN ans.ans_nft_owner as ao ON an.name_hash = ao.name_hash
                WHERE parent = $1";
    println!("get_subdomains_by_namehash query db: {} param {}", &query, &name_hash);
    let _rows = client.query(query, &[&name_hash]).await?;

    let mut subdomains = Vec::new();

    for row in _rows {
        let address: Option<String> = row.get(3);
        let address = match address {
            Some(addr) => addr,
            None => "".to_string(),
        };


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

    let query = "select ap.id, an.full_name from ans.ans_primary_name As ap
        LEFT JOIN ans.ans_name as an ON ap.name_hash = an.name_hash
        WHERE ap.address = $1 limit 1";

    println!("get_primary_name_by_address query db: {} address {}", &query, &address);
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[&address]).await?;
    Ok(row.get(1))
}

pub async fn get_hash_by_name(pool: &Data<Pool>, name: &String) -> Result<String, Error> {
    let client = pool.get().await.unwrap();
    let query = "select id, name_hash from ans.ans_name WHERE full_name = $1 limit 1";
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[&name]).await?;
    Ok(row.get(1))
}

pub async fn get_address_by_hash(pool: &Data<Pool>, name_hash: &String) -> Result<String, Error> {
    let client = pool.get().await.unwrap();
    let query = "select id, address from ans.ans_nft_owner WHERE name_hash = $1 limit 1";
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[&name_hash]).await?;
    Ok(row.get(1))
}