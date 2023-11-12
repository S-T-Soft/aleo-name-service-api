use actix_web::{App, get, HttpResponse, HttpServer, Responder, web};
use actix_cors::Cors;
use serde::Deserialize;
use serde_json;
// use deadpool_redis::{Config, Runtime, Pool};
use snarkvm_console_program::FromStr;
use tokio_postgres::NoTls;
use std::env;
use models::*;

mod utils;
mod client;
mod db;
mod models;

#[derive(Deserialize)]
struct GetResolverParams {
    name: String,
    category: String,
}

#[get("/name_to_hash/{name}")]
async fn name_to_hash(name: web::Path<String>) -> impl Responder {
    let name = name.into_inner();
    let name_hash = utils::parse_name_hash(&name);
    let name_hash = match name_hash {
        Ok(value) => value.to_string(),
        Err(e) => {
            println!("Error parsing name: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({ "error": format!("Error parsing name: {}", e) }));
        }
    };

    HttpResponse::Ok().json(NameHash { name_hash, name, })
}

#[get("/hash_to_name/{name_hash}")]
async fn hash_to_name(db_pool: web::Data<deadpool_postgres::Pool>, name_hash: web::Path<String>) -> impl Responder {
    let name_hash = name_hash.into_inner();
    let nft = db::get_name_by_namehash(&db_pool, &name_hash).await;

    match nft {
        Ok(data) => HttpResponse::Ok().json(NameHash {name_hash: data.name_hash, name: data.name}),
        Err(_e) => HttpResponse::NotFound().finish(),
    }
}

#[get("/primary_name/{address}")]
async fn name_api(db_pool: web::Data<deadpool_postgres::Pool>, address: web::Path<String>) -> impl Responder {
    let address = address.into_inner();
    let name = db::get_primary_name_by_address(&db_pool, &address);

    match name.await {
        Ok(name) => HttpResponse::Ok().json(AddressName { address: address.clone(), name }),
        Err(_e) => HttpResponse::NotFound().finish(),
    }
}

#[get("/address/{name}")]
async fn address_api(db_pool: web::Data<deadpool_postgres::Pool>, name: web::Path<String>) -> impl Responder {
    let name = name.into_inner();
    let name_hash = db::get_hash_by_name(&db_pool, &name).await;
    let name_hash = match name_hash {
        Ok(_hash) => _hash,
        Err(_e) => {
            return HttpResponse::NotFound().finish();
        }
    };
    let address = db::get_address_by_hash(&db_pool, &name_hash);

    match address.await {
        Ok(address) => HttpResponse::Ok().json(AddressName { address, name: name.clone() }),
        Err(_e) => HttpResponse::Ok().json(AddressName { address: "Private Registration".to_string(), name: name.clone() }),
    }
}


#[get("/resolver")]
async fn resolver(db_pool: web::Data<deadpool_postgres::Pool>, resolver_params: web::Query<GetResolverParams>) -> impl Responder {
    let name = resolver_params.name.clone();
    let category = resolver_params.category.clone();
    println!("name: {}, category: {}", name, category);

    let name_hash = db::get_hash_by_name(&db_pool, &name).await;
    let name_hash = match name_hash {
        Ok(_hash) => _hash,
        Err(_e) => {
            return HttpResponse::NotFound().finish();
        }
    };

    let nft = db::get_resolver(&db_pool, &name_hash, &category).await;

    match nft {
        Ok(nft) => HttpResponse::Ok().json(ResolverContent { content: nft.name, name: name.clone() , category: nft.category}),
        Err(_e) => HttpResponse::NotFound().finish(),
    }
}


#[get("/public_ans/{address}")]
async fn public_ans(db_pool: web::Data<deadpool_postgres::Pool>, address: web::Path<String>) -> impl Responder {
    let address = address.into_inner();
    let names = db::get_names_by_addr(&db_pool, &address).await;

    match names {
        Ok(data) => HttpResponse::Ok().json(data),
        Err(_e) => HttpResponse::NotFound().finish(),
    }
}

#[get("/resolver/{name}")]
async fn resolvers(db_pool: web::Data<deadpool_postgres::Pool>, name: web::Path<String>) -> impl Responder {
    let name = name.into_inner();

    let name_hash = db::get_hash_by_name(&db_pool, &name).await;
    let name_hash = match name_hash {
        Ok(_hash) => _hash,
        Err(_e) => {
            return HttpResponse::NotFound().finish();
        }
    };

    let name_resolvers = db::get_resolvers_by_namehash(&db_pool, &name_hash).await;

    match name_resolvers {
        Ok(data) => HttpResponse::Ok().json(data),
        Err(_e) => HttpResponse::NotFound().finish(),
    }
}


#[get("/subdomain/{name}")]
async fn subdomains(db_pool: web::Data<deadpool_postgres::Pool>, name: web::Path<String>) -> impl Responder {
    let name = name.into_inner();
    if "ans".eq(&name) {
        return HttpResponse::NotFound().finish();
    }

    let name_hash = db::get_hash_by_name(&db_pool, &name).await;
    let name_hash = match name_hash {
        Ok(_hash) => _hash,
        Err(_e) => {
            return HttpResponse::NotFound().finish();
        }
    };

    let name_subdomains = db::get_subdomains_by_namehash(&db_pool, &name_hash).await;

    match name_subdomains {
        Ok(data) => HttpResponse::Ok().json(data),
        Err(_e) => HttpResponse::NotFound().finish(),
    }
}

#[get("/token/{name_hash}.svg")]
async fn token_png(db_pool: web::Data<deadpool_postgres::Pool>, name_hash: web::Path<String>) -> impl Responder {
    let name_hash = name_hash.into_inner();
    if name_hash.is_empty() {
        return HttpResponse::NotFound().finish();
    }

    let nft = db::get_name_by_namehash(&db_pool, &name_hash);

    match nft.await {
        Ok(nft) => {
            let svg_content = include_str!("./file/demo.svg");
            HttpResponse::Ok()
                .content_type("image/svg+xml")
                .body(svg_content.replace("{aleonameservice}", &nft.name))
        },
        Err(_e) => HttpResponse::NotFound().finish(),
    }
}

#[get("/token/{name_hash}")]
async fn token(db_pool: web::Data<deadpool_postgres::Pool>, name_hash: web::Path<String>) -> impl Responder {
    let name_hash = name_hash.into_inner();
    if name_hash.is_empty() {
        return HttpResponse::NotFound().finish();
    }

    let nft = db::get_name_by_namehash(&db_pool, &name_hash);
    match nft.await {
        Ok(nft) => {
            let mut attributes = Vec::new();

            let level = nft.name.chars().filter(|&c| c == '.').count();
            attributes.push(AnsTokenAttr {trait_type: "level".to_string(), value: level.to_string() });
            let name_length = match nft.name.chars().position(|c| c == '.') {
                Some(index) => index,
                None => 0,
            };
            attributes.push(AnsTokenAttr {trait_type: "length".to_string(), value: name_length.to_string()});

            let ans_token = AnsToken {
                name: nft.name,
                image: format!("https://api.aleonames.id/token/{}.svg", &name_hash),
                attributes,
                mint_number: 1i32,
                collection_name: "ANS".to_string(),
                collection_link: "https://aleonames.id".to_string(),
                collection_description: "Aleo Name Service".to_string(),
                source_link: format!("https://api.aleonames.id/token/{}", &name_hash),
            };

            HttpResponse::Ok().json(ans_token)
        },
        Err(_e) => HttpResponse::NotFound().finish(),
    }
}



#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/0".to_string());
    // let redis_cfg = Config::from_url(redis_url);
    // let redis_pool = redis_cfg.create_pool(Some(Runtime::Tokio1)).unwrap();

    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://casaos:casaos@10.0.0.17:5432/aleoe".to_string());
    let db_config= tokio_postgres::Config::from_str(&db_url).unwrap();
    let mgr_config =deadpool_postgres::ManagerConfig {
        recycling_method: deadpool_postgres::RecyclingMethod::Fast
    };
    let db_mgr = deadpool_postgres::Manager::from_config(db_config, NoTls, mgr_config);
    let db_pool = deadpool_postgres::Pool::builder(db_mgr).max_size(24).build().unwrap();

    println!("start server listening in 0.0.0.0:8080");
    HttpServer::new(move || {
        App::new()
            .wrap(
                Cors::permissive()
                    .allow_any_origin()
            )
            // .app_data(web::Data::new(redis_pool.clone()))
            .app_data(web::Data::new(db_pool.clone()))
            .service(name_to_hash)
            .service(hash_to_name)
            .service(name_api)
            .service(address_api)
            .service(resolver)
            .service(public_ans)
            .service(resolvers)
            .service(subdomains)
            .service(token_png)
            .service(token)

            
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}
