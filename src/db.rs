use deadpool_postgres::Pool;
use serde::Serialize;
use tokio_postgres::Error;

#[derive(Serialize)]
pub struct NFT {
    pub name_hash: String,
    pub address: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct NFTWithPrimary {
    pub name_hash: String,
    pub address: String,
    pub name: String,
    pub is_primary_name: bool,
    pub resolver: String,
}

#[derive(Serialize)]
pub struct Resolver {
    pub name_hash: String,
    pub category: String,
    pub name: String,
    pub version: i32
}

// use pool client now
// async fn connect() -> Result<Client, Error> {
//     let conn_str = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
//     let (client, connection) = tokio_postgres::connect(&conn_str, NoTls).await?;
//     tokio::spawn(async move {
//         if let Err(e) = connection.await {
//             eprintln!("connection db err: {}", e)
//         }
//     });

//     Ok(client)
// }

pub async fn get_name_by_namehash(pool: &Pool, name_hash: &str) -> Result<NFTWithPrimary, Error> {
    let client = pool.get().await.unwrap();
    let query = "select an.id, an.name_hash, an.full_name, an.resolver, ap.id from ans.ans_name AS an 
            LEFT JOIN ans.ans_primary_name as ap ON an.name_hash = ap.name_hash
            WHERE an.name_hash = $1 limit 1";
    
    println!("get_name_by_namehash query db: {} params {}", &query, name_hash);
    let row = client.query_one(query, &[&name_hash]).await?;
    if row.is_empty() {
        eprintln!("empty")
    }

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
            resolver: resolver,
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
            address: address,
        };
        subdomains.push(r);
    }

    Ok(subdomains)
}