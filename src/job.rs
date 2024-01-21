use std::env;
use std::str::FromStr;
use std::time::Duration;
use deadpool_redis::{Config as RedisConfig, Pool as RedisPool, Runtime as RedisRuntime};
use deadpool_redis::redis::cmd;
use tokio::time::{sleep, timeout};
use tokio_postgres::NoTls;
use tracing::{error, info};
use crate::{client, db};

pub async fn run() {
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/0".to_string());
    let redis_cfg = RedisConfig::from_url(redis_url);
    let redis_pool = redis_cfg.create_pool(Some(RedisRuntime::Tokio1)).unwrap();

    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://casaos:casaos@10.0.0.17:5432/aleoe".to_string());
    let db_config= tokio_postgres::Config::from_str(&db_url).unwrap();
    let mgr_config =deadpool_postgres::ManagerConfig {
        recycling_method: deadpool_postgres::RecyclingMethod::Fast
    };
    let db_mgr = deadpool_postgres::Manager::from_config(db_config, NoTls, mgr_config);
    let db_pool = deadpool_postgres::Pool::builder(db_mgr).max_size(2).build().unwrap();

    loop {
        job_get_statistic_data(&redis_pool, &db_pool).await;
        sleep(Duration::from_secs(10)).await;
        job_get_statistic_data(&redis_pool, &db_pool).await;

        let timeout_duration = Duration::from_secs(10);
        let job1 = timeout(timeout_duration, job_get_api_height(&redis_pool));
        let job2 = timeout(timeout_duration, job_get_indexer_height(&redis_pool, &db_pool));
        tokio::try_join!(job1, job2).expect("Failed to join jobs");

        sleep(Duration::from_secs(10)).await;
        job_get_statistic_data(&redis_pool, &db_pool).await;
        sleep(Duration::from_secs(10)).await;
        info!("loop run jobs every 30 seconds");
    }
}

async fn job_get_indexer_height(redis_pool: &RedisPool, db_pool: &deadpool_postgres::Pool) {
    let client = db_pool.get().await.unwrap();
    let mut conn = redis_pool.get().await.unwrap();

    let query = "select height from ans3.block order by height desc limit 1";
    let query = client.prepare(&query).await.unwrap();
    let row = client.query_one(&query, &[]).await.unwrap();
    if !row.is_empty() {
        let height:i64 = row.get(0);
        info!("set indexer:height: {}", height);
        let _: () = cmd("SET").arg("indexer:height").arg(height).query_async(&mut conn).await.expect("set indexer height fail");
    }
}

async fn job_get_api_height(redis_pool: &RedisPool) {
    let mut conn = redis_pool.get().await.unwrap();
    match client::get_last_height().await {
        Ok(height) => {
            info!("set cache:api_height: {}", height);
            let _: () = cmd("SET").arg("cache:api_height").arg(height).query_async(&mut conn).await.expect("set api height fail");
        }
        _ => {}
    }
}

async fn job_get_statistic_data(redis_pool: &RedisPool, db_pool: &deadpool_postgres::Pool) {
    let mut conn = redis_pool.get().await.unwrap();

    match db::get_statistic_data(db_pool).await {
        Ok(data) => {
            info!("job_get_statistic_data success!");
            let key = "cache:statistic";
            let data_json = serde_json::to_string(&data).expect("Failed get statistic json");
            let _: () = cmd("SET").arg(key).arg(data_json)
                .query_async(&mut conn).await
                .expect("Failed to set key-value");

            // 设置过期时间
            let _: () = cmd("EXPIRE").arg(key).arg(3600)
                .query_async(&mut conn).await
                .expect("Failed to set expiration time");
        }
        Err(e) => {
            error!("job_get_statistic_data fail: {}", e);
        },
    }
}