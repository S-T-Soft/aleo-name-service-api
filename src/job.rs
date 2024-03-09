use std::env;
use std::str::FromStr;
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tokio_postgres::NoTls;
use tracing::{error, info, instrument, warn};
use crate::{client, db};

pub async fn run() {

    let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://casaos:casaos@10.0.0.17:5432/aleoe".to_string());
    let db_config= tokio_postgres::Config::from_str(&db_url).unwrap();
    let mgr_config =deadpool_postgres::ManagerConfig {
        recycling_method: deadpool_postgres::RecyclingMethod::Fast
    };
    let db_mgr = deadpool_postgres::Manager::from_config(db_config, NoTls, mgr_config);
    let db_pool = deadpool_postgres::Pool::builder(db_mgr).max_size(2).build().unwrap();

    loop {
        job_get_statistic_data(&db_pool).await;
        sleep(Duration::from_secs(10)).await;
        job_get_statistic_data(&db_pool).await;

        let timeout_duration = Duration::from_secs(15);
        let job1 = timeout(timeout_duration, job_get_api_height(&db_pool));
        let job3 = timeout(timeout_duration, job_get_api_host(&db_pool));
        if let Err(err) = tokio::try_join!(job1, job3) {
            warn!("fail join tasks!!! {}", err);
        }

        sleep(Duration::from_secs(10)).await;
        job_get_statistic_data(&db_pool).await;
        sleep(Duration::from_secs(10)).await;
        info!("loop run jobs every 30 seconds");
    }
}

async fn job_get_api_height(db_pool: &deadpool_postgres::Pool) {
    match client::get_last_height().await {
        Ok(height) => {
            info!("set cache:api_height: {}", height);
            db::set_kv_value(db_pool, "api_height", &height.to_string()).await;
        }
        _ => {error!("set cache:api_height fail!!");}
    }
}

async fn job_get_statistic_data(db_pool: &deadpool_postgres::Pool) {
    match db::get_statistic_data(db_pool).await {
        Ok(data) => {
            info!("job_get_statistic_data success!");
            let key = "api_statistic";
            let data_json = serde_json::to_string(&data).expect("Failed get statistic json");
            db::set_kv_value(db_pool, key, &data_json).await;
        }
        Err(e) => {
            error!("job_get_statistic_data fail: {}", e);
        },
    }
}

async fn job_get_api_host(db_pool: &deadpool_postgres::Pool) {
    let api_hosts = env::var("URL_HOSTS").unwrap_or_else(|_| "".to_string());
    if api_hosts.is_empty() {
        return;
    }
    let mut max_height = 0;
    let mut max_url = "".to_string();

    for url in api_hosts.split(',') {
        let height_resp = get_last_height(url).await;
        match height_resp {
            Ok(height) => {
                if  height > 0 && height > max_height {
                    max_height = height;
                    max_url = url.to_string();
                }
            }
            Err(_) => {
                warn!("job_get_api_host:get_last_height fail {}", url)
            }
        }
    }
    if max_height > 0 && !max_url.is_empty() {
        env::set_var("URL_HOST", &max_url);
        db::set_kv_value(db_pool, "api_host", &max_url).await;
        info!("job_get_api_host success : {}", &max_url);
    }

}


#[instrument]
pub async fn get_last_height(api_host: &str) -> Result<u32, String> {
    let url = format!("{}/testnet3/block/height/latest", api_host);
    let resp = client::call_api(url).await?;
    let height: u32 = resp.parse().unwrap_or_else(|_| 0);
    Ok( height)
}