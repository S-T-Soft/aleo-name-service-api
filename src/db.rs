use std::env;
use serde::Serialize;
use tokio_postgres::{NoTls, Error, Client};

#[derive(Serialize)]
pub struct NFT {
    name_hash: String,
    address: String,
    name: String,
}

#[derive(Serialize)]
pub struct NFTWithPrimary {
    name_hash: String,
    address: String,
    name: String,
    is_primary_name: bool,
}

#[derive(Serialize)]
pub struct Resolver {
    name_hash: String,
    category: String,
    name: String,
    version: i64
}

async fn connect() -> Result<Client, Error> {
    let conn_str = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let (client, connection) = tokio_postgres::connect(&conn_str, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("connection db err: {}", e)
        }
    });

    Ok(client)
}

pub async fn get_names_by_addr(address: &str) -> Result<Vec<NFTWithPrimary>, Error> {
    let client = connect().await?;
    let query = format!("select ao.id, ao.name_hash, ao.address, an.name, ap.id from ans.ans_nft_owner as ao 
            JOIN ans.ans_name as an ON ao.name_hash = an.name_hash
            LEFT JOIN ans.ans_primary_name as ap ON ao.name_hash = ap.name_hash
            where ao.address = '{}'", address);
    let _rows = client.query(&query, &[]).await?;

    println!("query db: {}", query);
    let mut nft_list = Vec::new();

    for row in _rows {
        let primary_id: Option<i64> = row.get(4);
        let is_primary = match primary_id {
            Some(pid) => pid > 0,
            None => false,
        };
        
        let nft = NFTWithPrimary {
            name_hash: row.get(1),
            address: row.get(2),
            name: row.get(3),
            is_primary_name: is_primary,
        };
        nft_list.push(nft);
    }

    Ok(nft_list)
}

pub async fn get_resolvers_by_namehash(name_hash: &str) -> Result<Vec<Resolver>, Error> {
    let client = connect().await?;
    let query = format!("select id,category,version,name from ans.ans_resolver where name_hash = '{}'", name_hash);
    let _rows = client.query(&query, &[]).await?;

    println!("query db: {}", query);
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

pub async fn get_subdomains_by_namehash(name_hash: &str) -> Result<Vec<NFT>, Error> {
    let client = connect().await?;
    let query = format!("select an.id, an.name_hash, an.name, ao.address from ans.ans_name as an
            LEFT JOIN ans.ans_nft_owner as ao ON an.name_hash = ao.name_hash
                WHERE parent = '{}'", name_hash);
    let _rows = client.query(&query, &[]).await?;

    println!("query db: {}", query);
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
            address: address,
        };
        subdomains.push(r);
    }

    Ok(subdomains)
}