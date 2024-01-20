use actix_web::{App, get, HttpResponse, HttpServer, Responder, web};
use actix_cors::Cors;
use serde::Deserialize;
use serde_json;
use deadpool_redis::{Config as RedisConfig, Runtime as RedisRuntime};
use snarkvm_console_program::FromStr;
use tokio_postgres::NoTls;
use std::env;
use actix_web_prom::PrometheusMetricsBuilder;
use base64::encode;
use deadpool_redis::redis::cmd;
use reqwest::StatusCode;
use actix_governor::{Governor, GovernorConfigBuilder};
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt, Registry};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use models::*;

mod utils;
mod client;
mod db;
mod models;
mod auth;
mod indexer;
mod job;

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
            warn!("Error parsing name: {} .. {}", &name, e);
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
async fn address_api(db_pool: web::Data<deadpool_postgres::Pool>, redis_pool: web::Data<deadpool_redis::Pool>, name: web::Path<String>) -> impl Responder {
    let name = name.into_inner();
    let name_hash = db::get_hash_by_name(&db_pool, &name).await;
    let name_hash = match name_hash {
        Ok(_hash) => _hash,
        Err(_e) => {
            if client::is_n_query_from_api(&redis_pool).await {
                match client::check_name_hash(&name).await {
                    Ok(v) => v.to_string(),
                    Err(_) => "".to_string()
                }
            } else {
                "".to_string()
            }
        }
    };
    if name_hash.is_empty() {
        return HttpResponse::NotFound().finish();
    }
    let address = db::get_address_by_hash(&db_pool, &name_hash);

    match address.await {
        Ok(address) => HttpResponse::Ok().json(AddressName { address, name: name.clone() }),
        Err(_e) => {
            if client::is_n_query_from_api(&redis_pool).await {
                return match client::get_owner(&name_hash).await {
                    Ok(address) => HttpResponse::Ok().json(AddressName { address, name: name.clone() }),
                    Err(_) => HttpResponse::Ok().json(AddressName { address: "Private Registration".to_string(), name: name.clone() })
                };
            }

            HttpResponse::Ok().json(AddressName { address: "Private Registration".to_string(), name: name.clone() })
        },
    }
}


#[get("/resolver")]
async fn resolver(db_pool: web::Data<deadpool_postgres::Pool>, resolver_params: web::Query<GetResolverParams>) -> impl Responder {
    let name = resolver_params.name.clone();
    let category = resolver_params.category.clone();
    info!("name: {}, category: {}", &name, &category);

    let name_hash = db::get_hash_by_name(&db_pool, &name).await;
    let name_hash = match name_hash {
        Ok(_hash) => _hash,
        Err(_e) => {
            return HttpResponse::NotFound().finish();
        }
    };

    let resolver_result = db::get_resolver(&db_pool, &name_hash, &category).await;

    match resolver_result {
        Ok(nft) => HttpResponse::Ok().json(ResolverContent {
                name_hash: nft.name_hash,
                category: nft.category,
                version: nft.version,
                content: nft.content,
                name
            }),
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

#[get("/resolvers/{name}")]
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
        Ok(data) => HttpResponse::Ok().json(data.into_iter().map(|nft| ResolverContent{
            name_hash: nft.name_hash,
            category: nft.category,
            version: nft.version,
            content: nft.content, name: name.clone()}).collect::<Vec<_>>()),
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
            let avatar_url = match db::get_resolver(&db_pool, &name_hash, "avatar").await {
                Ok(r) => format!("https://ipfs.io/ipfs/{}", r.content.replace("ipfs://", "")),
                Err(_e) => "".to_string()
            };
            let mut fill_bg = "paint0_linear";
            let mut fill_bg_base64 = "".to_string();
            if !avatar_url.is_empty() {
                info!("image url: {}", &avatar_url);
                let response = reqwest::get(&avatar_url).await.unwrap();
                if response.status() == StatusCode::OK {
                    let resp_headers = response.headers().clone();
                    let base64_image = encode(&response.bytes().await.unwrap().to_vec());
                    let image_type = resp_headers.get("Content-Type").unwrap();
                    fill_bg_base64 = format!("data:{};base64,{}", image_type.to_str().unwrap(), base64_image);
                    fill_bg = "bg-image";
                } else {
                    // 请求失败，处理错误情况
                }
            }

            let svg_content = include_str!("./file/demo.svg");
            let name_parts = utils::split_string(&nft.name);
            let mut name_texts = Vec::new();
            for i in 0..name_parts.len() {
                let mut dy = 0.0;
                if name_parts.len() > 1 {
                    dy = if i == 0 { -1.2 * (name_parts.len() - 1) as f64 } else { 1.2 };
                }
                let name_text = format!("<tspan x=\"26\" dy=\"{}em\">{}</tspan>", dy, name_parts.get(i).unwrap());
                name_texts.push(name_text);
            }

            HttpResponse::Ok()
                .content_type("image/svg+xml")
                .body(svg_content.replace("{fill_bg}", &fill_bg).replace("{aleonameservice}", &name_texts.join("")).replace("{fill_bg_base64}", &fill_bg_base64))
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
                None => nft.name.len(),
            };
            attributes.push(AnsTokenAttr {trait_type: "length".to_string(), value: name_length.to_string()});

            let ans_api_url = env::var("ANS_API_URL").unwrap_or_else(|_| "https://api.aleonames.id".to_string());
            let ans_token = AnsToken {
                name: nft.name,
                image: format!("{}/token/{}.svg", &ans_api_url, &name_hash),
                attributes,
                mint_number: 1i32,
                collection_name: "ANS".to_string(),
                collection_link: "https://aleonames.id".to_string(),
                collection_description: "Aleo Name Service".to_string(),
                source_link: format!("{}/token/{}", &ans_api_url, &name_hash),
            };

            HttpResponse::Ok().json(ans_token)
        },
        Err(_e) => HttpResponse::NotFound().finish(),
    }
}

#[get("/statistic")]
async fn statistic(db_pool: web::Data<deadpool_postgres::Pool>, redis_pool: web::Data<deadpool_redis::Pool>) -> impl Responder {

    let mut conn = redis_pool.get().await.unwrap();
    let cached_value: Option<String> = match cmd("GET").arg(&["cache:statistic"]).query_async(&mut conn).await {
        Ok(value) => value,
        Err(_) => None
    };
    if let Some(value) = cached_value {
        let cached_data: AnsStatistic = serde_json::from_str(&value).expect("failed get cached data");
        return HttpResponse::Ok().json(cached_data);
    }

    let statistic_data = db::get_statistic_data(&db_pool).await;
    match statistic_data {
        Ok(data) => {
            let key = "cache:statistic";
            let data_json = serde_json::to_string(&data).expect("Failed get statistic json");
            let _: () = cmd("SET")
                .arg(key)
                .arg(data_json)
                .query_async(&mut conn)
                .await
                .expect("Failed to set key-value");

            // 设置过期时间
            let _: () = cmd("EXPIRE")
                .arg(key)
                .arg(10)
                .query_async(&mut conn)
                .await
                .expect("Failed to set expiration time");

            HttpResponse::Ok().json(data)
        }
        Err(e) => {
            eprintln!("statistic fail: {}", e);
            HttpResponse::NotFound().finish()
        },
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    init_tracing();

    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/0".to_string());
    let redis_cfg = RedisConfig::from_url(redis_url);
    let redis_pool = redis_cfg.create_pool(Some(RedisRuntime::Tokio1)).unwrap();

    // db config
    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://casaos:casaos@10.0.0.17:5432/aleoe".to_string());
    let db_config= tokio_postgres::Config::from_str(&db_url).unwrap();
    let mgr_config =deadpool_postgres::ManagerConfig {
        recycling_method: deadpool_postgres::RecyclingMethod::Fast
    };
    let db_mgr = deadpool_postgres::Manager::from_config(db_config, NoTls, mgr_config);
    let db_pool = deadpool_postgres::Pool::builder(db_mgr).max_size(24).build().unwrap();

    // prometheus config
    let prometheus = PrometheusMetricsBuilder::new("api")
        .endpoint("/metrics")
        .build()
        .unwrap();

    let governor_conf = GovernorConfigBuilder::default()
        .per_second(2)
        .burst_size(32)
        .finish()
        .unwrap();

    tokio::spawn(async {
        indexer::sync_data().await;
    });

    tokio::spawn(async {
        job::run().await;
    });

    println!("start server listening in 0.0.0.0:8080");
    HttpServer::new(move || {
        App::new()
            .wrap(
                Cors::permissive()
                    .allow_any_origin()
            )
            .wrap(prometheus.clone())
            .wrap(auth::Authentication)
            .wrap(Governor::new(&governor_conf))
            .app_data(web::Data::new(redis_pool.clone()))
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
            .service(statistic)

            
    })
    .bind("0.0.0.0:8080")?
    .run()
    .await
}

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let formatting_layer = fmt::layer().with_writer(std::io::stderr);
    Registry::default().with(env_filter).with(formatting_layer).init();
}
