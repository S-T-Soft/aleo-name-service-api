use std::{env, fmt};
use std::cmp::{max, min};
use std::error::Error;
use std::str::FromStr;
use std::time::Duration;
use lazy_static::lazy_static;
use regex::Regex;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use serde::Deserialize;
use tokio::time::sleep;
use snarkvm_console_network::Network;
use snarkvm_ledger_block::{Block, Transaction};
use snarkvm_console_network::prelude::ToBytes;
use snarkvm_console_program::{Field, Address, Argument, FromBytes};
use snarkvm_ledger_block::{Transition};
use tokio_postgres::NoTls;
use tracing::{error, info};
use crate::{client, utils};
use crate::db::{get_kv_value, set_kv_value};

static MAX_BLOCK_RANGE: u32 = 50;
const CDN_ENDPOINT: &str = "https://s3.us-west-1.amazonaws.com/testnet.blocks/phase3";
const INDEXER_HEIGHT_KEY: &str = "indexer_height";

#[derive(Debug)]
struct IndexError(Box<dyn Error>);

lazy_static! {
    static ref BLOCKS_PRE_ROUND: i64 = {
        let value = env::var("BLOCKS_PRE_ROUND")
            .unwrap_or_else(|_| "20".to_string());
        value.parse::<i64>()
            .expect("Cannot parse BLOCKS_PRE_ROUND env var")
    };
    static ref ANS_BLOCK_HEIGHT_START: i64 = {
        let value = env::var("ANS_BLOCK_HEIGHT_START")
            .unwrap_or_else(|_| "1".to_string());
        value.parse::<i64>()
            .expect("Cannot parse ANS_BLOCK_HEIGHT_START env var")
    };
    static ref PROGRAM_ID: String = env::var("PROGRAM_ID").unwrap_or_else(|_| "aleo_name_service_registry".to_string());
    static ref REGISTER: &'static str = "register";
    static ref REGISTER_TLD: &'static str = "register_tld";
    static ref REGISTER_PRIVATE: &'static str = "register_private";
    static ref REGISTER_PUBLIC: &'static str = "register_public";
    static ref TRANSFER_PUBLIC: &'static str = "transfer_public";
    static ref TRANSFER_PRIVATE: &'static str = "transfer_private";
    static ref TRANSFER_PRIVATE_TO_PUBLIC: &'static str = "transfer_private_to_public";
    static ref TRANSFER_PUBLIC_TO_PRIVATE: &'static str = "transfer_public_to_private";
    static ref TRANSFER_FROM_PUBLIC: &'static str = "transfer_from_public";
    static ref SET_PRIMARY_NAME: &'static str = "set_primary_name";
    static ref UNSET_PRIMARY_NAME: &'static str = "unset_primary_name";
    static ref SET_RESOLVER: &'static str = "set_resolver";
    static ref BURN: &'static str = "burn";

    static ref RECORD_PROGRAM_ID: String = env::var("RECORD_PROGRAM_ID").unwrap_or_else(|_| "ans_resolver".to_string());
    static ref SET_RESOLVER_RECORD: &'static str = "set_resolver_record";
    static ref UNSET_RESOLVER_RECORD: &'static str = "unset_resolver_record";
    static ref SET_RESOLVER_RECORD_PUBLIC: &'static str = "set_resolver_record_public";
    static ref UNSET_RESOLVER_RECORD_PUBLIC: &'static str = "unset_resolver_record_public";

    static ref TRANSFER_PROGRAM_ID: String = env::var("TRANSFER_PROGRAM_ID").unwrap_or_else(|_| "ans_credit_transfer".to_string());
    static ref TRANSFER_CREDITS: &'static str = "transfer_credits";
    static ref TRANSFER_CREDITS_PUBLIC: &'static str = "transfer_credits_public";
    static ref CLAIM_CREDITS_PUBLIC: &'static str = "claim_credits_public";
    static ref CLAIM_CREDITS_PRIVATE: &'static str = "claim_credits_private";
    static ref CLAIM_CREDITS_AS_SIGNER: &'static str = "claim_credits_as_signer";
    static ref TRANSFER_TOKEN: &'static str = "transfer_token";
    static ref TRANSFER_TOKEN_PUBLIC: &'static str = "transfer_token_public";
    static ref CLAIM_TOKEN_PUBLIC: &'static str = "claim_token_public";
    static ref CLAIM_TOKEN_PRIVATE: &'static str = "claim_token_private";
    static ref CLAIM_TOKEN_AS_SIGNER: &'static str = "claim_token_as_signer";

    static ref DB_POOL: deadpool_postgres::Pool = {
        let db_url = env::var("DATABASE_URL").unwrap();
        let db_config= tokio_postgres::Config::from_str(&db_url).unwrap();
        let mgr_config =deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Fast
        };
        let db_mgr = deadpool_postgres::Manager::from_config(db_config, NoTls, mgr_config);
        deadpool_postgres::Pool::builder(db_mgr).max_size(3).build().unwrap()
    };
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "IndexError Error: {}", self.0)
    }
}

impl Error for IndexError {}

fn preprocess_json(json_data: &str) -> String {
    let re = Regex::new(r#""(cumulative_weight|cumulative_proof_target)":\s*([\d]+)"#).unwrap();

    let result = re.replace_all(json_data, r#""$1": "$2""#);

    result.to_string()
}

pub async fn sync_data<N: Network>() {
    info!("Start data indexer...");
    let mut latest_height = get_latest_height().await as i64;
    // match sync_from_cdn(latest_height).await {
    //     Ok(_) => info!("sync from cdn finished!"),
    //     _ => {}
    // }

    fix_transfer_key().await;

    let mut batch_retry_delay_ms: u64 = 500;
    let mut single_retry_delay_ms: u64 = 500;

    loop {
        latest_height = match get_kv_value(&DB_POOL, "api_height").await {
            Ok(v) => max(v.parse().unwrap(), latest_height),
            Err(_) => 0i64
        };

        let (block_number, latest_height) = get_next_block_number(latest_height).await.unwrap_or_else(|e| {
            eprintln!("Error fetching next block number: {}", e);
            (0, latest_height)
        });

        if block_number > 0 {
            let to_block = min(latest_height, block_number + *BLOCKS_PRE_ROUND) as u32;
            info!("Syncing data from block {} to {}", block_number, to_block);
            if (block_number as u32) < to_block {
                match client::get_blocks(block_number as u32, to_block).await {
                    Ok(response) => {
                        batch_retry_delay_ms = 500;
                        let response = preprocess_json(&response);
                        match serde_json::from_str::<Vec<serde_json::Value>>(&response) {
                            Ok(blocks_json) => {
                                for (i, block_value) in blocks_json.iter().enumerate() {
                                    let block_json = block_value.to_string();
                                    let height = block_number as u32 + i as u32;
                                    if let Err(e) = index_data::<N>(&block_json, height).await {
                                        error!("Error indexing block {}: {}. Retrying in {}ms", height, e, batch_retry_delay_ms);
                                        sleep(Duration::from_millis(batch_retry_delay_ms)).await;
                                        break;
                                    }
                                }
                            },
                            Err(e) => error!(
                                "Error parse batch response: {}, json head: {}",
                                e,
                                &response.chars().take(20).collect::<String>()
                            )
                        }
                    },
                    Err(e) => {
                        let delay = batch_retry_delay_ms;
                        batch_retry_delay_ms = std::cmp::min(batch_retry_delay_ms * 2, 16000);
                        error!("Error fetching batch data: {}. Retrying in {}ms", e, delay);
                        sleep(Duration::from_millis(delay)).await;
                    },
                }
            } else {
                match client::get_block(block_number as u32).await {
                    Ok(response) => {
                        single_retry_delay_ms = 500;
                        let response = preprocess_json(&response);
                        if let Err(e) = index_data::<N>(&response, block_number as u32).await {
                            error!("Error indexing block {}: {}. Retrying in {}ms", block_number, e, single_retry_delay_ms);
                            sleep(Duration::from_millis(single_retry_delay_ms)).await;
                        }
                    },
                    Err(e) => {
                        let delay = single_retry_delay_ms;
                        single_retry_delay_ms = std::cmp::min(single_retry_delay_ms * 2, 16000);
                        error!("Error fetching data: {}. Retrying in {}ms", e, delay);
                        sleep(Duration::from_millis(delay)).await;
                    },
                }
            }

            sleep(Duration::from_micros(50)).await;
        } else {
            sleep(Duration::from_secs(1)).await;
        }
    }
}


async fn fix_transfer_key() {
    let db_client = DB_POOL.get().await.unwrap();
    let db_schema = env::var("DB_SCHEMA").unwrap_or_else(|_| "ansb".to_string());
    db_client.execute(format!("SET search_path TO {db_schema}").as_str(), &[]).await.unwrap();

    let query = "select name_hash from ans_name where transfer_key is null";
    let query = db_client.prepare(&query).await.unwrap();
    let rows = db_client.query(&query, &[]).await.unwrap();

    for row in rows {
        let name_hash: String = row.get(0);
        let transfer_key = utils::get_name_hash_transfer_key(&name_hash).unwrap().to_string();
        info!("fix_transfer_key: {} {}", name_hash, transfer_key);
        db_client.execute("update ans_name set transfer_key=$1 where name_hash=$2",
                          &[&transfer_key, &name_hash]).await.unwrap();
    }
}

// async fn sync_from_cdn(init_latest_height: u32) -> Result<(), Box<dyn Error>> {
//     let block_number = match get_next_block_number(init_latest_height).await {
//         Ok(number) => number,
//         Err(e) => {
//             return Err(Box::new(IndexError(e)));
//         }
//     };
//
//     // get latest height from CDN
//     let latest_cdn_height = match client::get_cdn_last_height().await {
//         Ok(height) => height,
//         Err(err) => {
//             error!("get_latest_height error: {}", err);
//             init_latest_height
//         }
//     };
//
//     // local block height
//     let start = block_number as u32;
//     let end = std::cmp::min(latest_cdn_height, init_latest_height);
//     let total_blocks = end.saturating_sub(start);
//
//     info!("Sync {total_blocks} blocks from CDN (0% complete)...");
//
//     let mut current_start = start;
//     let batch_size = 1000u32;
//
//     while current_start < end {
//         let current_end = std::cmp::min(current_start + batch_size, end);
//
//         let cdn_request_start = current_start.saturating_sub(current_start % MAX_BLOCK_RANGE);
//         let cdn_request_end = current_end.saturating_sub(current_end % MAX_BLOCK_RANGE);
//         if cdn_request_end == cdn_request_start {
//             break;
//         }
//
//         let blocks_to_process = Arc::new(Mutex::new(Vec::new()));
//         let blocks_to_process_clone = blocks_to_process.clone();
//
//         info!("Sync blocks [{cdn_request_start} to {cdn_request_end}] from CDN");
//         let _shutdown = Default::default();
//         // Scan the blocks via the CDN.
//         let _ = snarkos_node_cdn::load_blocks(
//             &CDN_ENDPOINT,
//             cdn_request_start,
//             Some(cdn_request_end),
//             _shutdown,
//             move |block| {
//                 let mut blocks = blocks_to_process_clone.lock().unwrap();
//                 blocks.push(block);
//                 Ok(())
//             },
//         ).await;
//
//         let blocks = blocks_to_process.lock().unwrap().clone();
//         let expected_block_count = if cdn_request_end - cdn_request_start < batch_size {
//             cdn_request_end - cdn_request_start
//         } else {
//             batch_size
//         } as usize;
//
//         if blocks.len() == expected_block_count {
//             let mut block_stream = stream::iter(blocks);
//             while let Some(block) = block_stream.next().await {
//                 if block.height() >= start && block.height() <= end {
//                     index_data(&block).await;
//                 }
//             }
//             let percentage_complete =
//                 cdn_request_end.saturating_sub(start) as f64 * 100.0 / total_blocks as f64;
//             info!("Sync {total_blocks} blocks from CDN ({percentage_complete:.2}% complete)...");
//             current_start = cdn_request_end;
//         } else {
//             warn!("Incomplete batch detected, expected {} blocks, got {}. Retrying...", expected_block_count, blocks.len());
//             // Do not update current_start to retry the same batch
//         }
//     }
//
//     Ok(())
// }

async fn get_latest_height() -> u32 {
    loop {
        match client::get_last_height().await {
            Ok(height) => return height,
            Err(err) => {
                error!("get_latest_height error: {}", err);
                sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn get_next_block_number(init_latest_height: i64) -> Result<(i64, i64), Box<dyn Error>> {
    let mut local_latest_height = *ANS_BLOCK_HEIGHT_START;
    let db_client = DB_POOL.get().await?;
    let db_schema = env::var("DB_SCHEMA").unwrap_or_else(|_| "ansb".to_string());
    db_client.execute(format!("SET search_path TO {db_schema}").as_str(), &[]).await.unwrap();

    if let Ok(value) = get_kv_value(&DB_POOL, INDEXER_HEIGHT_KEY).await {
        local_latest_height = max(value.parse().unwrap_or(0), local_latest_height);
    }

    let query = "select height from block order by height desc limit 1";
    let query = db_client.prepare(&query).await.unwrap();
    let rows = db_client.query(&query, &[]).await?;
    if !rows.is_empty() {
        local_latest_height = max(rows.get(0).unwrap().get(0), local_latest_height);
    }

    let mut latest_height= init_latest_height;
    if local_latest_height >= latest_height || latest_height - local_latest_height < 11 {
        latest_height = get_latest_height().await as i64;
        if latest_height > init_latest_height {
            set_kv_value(&DB_POOL, "api_height", &latest_height.to_string()).await;
        }
    }

    info!("Latest height: {}", latest_height);

    let height = if latest_height as i64 > local_latest_height {
        local_latest_height + 1
    } else {
        0
    };
    Ok((height, latest_height))
}

struct BlockBasicInfo {
    height: u32,
    hash: String,
    previous_hash: String,
    timestamp: i64,
    has_relevant_programs: bool,
}

fn extract_block_basic_info(block_json: &str) -> Result<BlockBasicInfo, serde_json::Error> {
    #[derive(Debug, Deserialize)]
    struct BlockFilter {
        block_hash: String,
        previous_hash: String,
        header: BlockHeaderFilter,
        transactions: Vec<TransactionFilter>,
    }

    #[derive(Debug, Deserialize)]
    struct BlockHeaderFilter {
        metadata: MetadataFilter,
    }

    #[derive(Debug, Deserialize)]
    struct MetadataFilter {
        height: u32,
        timestamp: i64,
    }

    #[derive(Debug, Deserialize)]
    struct TransactionFilter {
        status: String,
        #[serde(rename = "type")]
        tx_type: String,
        transaction: TransactionDetailFilter,
    }

    #[derive(Debug, Deserialize)]
    struct TransactionDetailFilter {
        #[serde(default)]
        execution: Option<ExecutionFilter>,
    }

    #[derive(Debug, Deserialize)]
    struct ExecutionFilter {
        #[serde(default)]
        transitions: Vec<TransitionFilter>,
    }

    #[derive(Debug, Deserialize)]
    struct TransitionFilter {
        program: String,
    }

    // Parse JSON
    let block_filter: BlockFilter = match serde_json::from_str(block_json) {
        Ok(bf) => bf,
        Err(e) => {
            error!(
                "Error parsing block_filter: {}, json head: {}",
                e,
                &block_json.chars().take(20).collect::<String>()
            );
            return Err(e);
        }
    };

    // Check if the block contains relevant programs
    let has_relevant_programs = block_filter.transactions.iter().any(|tx| {
        tx.status == "accepted" &&
        tx.tx_type == "execute" &&
        tx.transaction.execution.as_ref().map_or(false, |exec| {
            exec.transitions.iter().any(|transition| {
                let program_name = transition.program.strip_suffix(".aleo")
                    .unwrap_or(&transition.program);
                program_name == *PROGRAM_ID ||
                    program_name == *RECORD_PROGRAM_ID ||
                    program_name == *TRANSFER_PROGRAM_ID
            })
        })
    });

    Ok(BlockBasicInfo {
        height: block_filter.header.metadata.height,
        hash: block_filter.block_hash,
        previous_hash: block_filter.previous_hash,
        timestamp: block_filter.header.metadata.timestamp,
        has_relevant_programs,
    })
}

async fn index_data<N: Network>(block_json: &str, block_height: u32) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("Process block at height {}", block_height);
    let mut db_client = DB_POOL.get().await?;
    let db_schema = env::var("DB_SCHEMA").unwrap_or_else(|_| "ansb".to_string());
    db_client.execute(format!("SET search_path TO {db_schema}").as_str(), &[]).await?;
    let db_trans = db_client.transaction().await?;
    if let Ok(basic_info) = extract_block_basic_info(block_json) {
        if basic_info.has_relevant_programs {
            db_trans.execute(
                "INSERT INTO block (height, block_hash, previous_hash, timestamp) VALUES ($1, $2,$3, $4) ON CONFLICT (height) DO NOTHING",
                &[&(basic_info.height as i64), &basic_info.hash, &basic_info.previous_hash, &basic_info.timestamp]
            ).await?;
            match serde_json::from_str::<Block<N>>(block_json) {
                Ok(block) => {
                    if let Err(e) = process_block_data(&db_trans, &block).await {
                        error!("Error processing block data: {}", e);
                        db_trans.rollback().await?;
                        return Err(e.into());
                    }
                },
                Err(e) => {
                    error!("Error parsing block: {}, json head: {}", e, &block_json.chars().take(20).collect::<String>());
                    db_trans.rollback().await?;
                    return Err(e.into());
                }
            }
        } else {
            info!("Block {} contains no relevant programs, skipping detailed parsing", block_height);
        }
        set_indexer_height(&db_trans, basic_info.height as i64).await?;
    } else {
        error!("Error extracting basic info from block JSON");
        db_trans.rollback().await?;
        return Err("Error extracting basic info from block JSON".into());
    }
    db_trans.commit().await?;
    Ok(())
}

async fn set_indexer_height(db_trans: &tokio_postgres::Transaction<'_>, height: i64) -> Result<(), tokio_postgres::Error> {
    let value = height.to_string();

    db_trans.execute(
        "INSERT INTO kv (key, value) VALUES ($1, $2) \
         ON CONFLICT (key) DO UPDATE SET value = $2, updated = EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT",
        &[&INDEXER_HEIGHT_KEY, &value]
    ).await?;
    Ok(())
}

async fn process_block_data<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("Process detailed data for block {} on {}", block.height(), block.timestamp());

    for transaction in block.transactions().clone().into_iter() {
        if transaction.is_accepted() {
            for transition in transaction.transitions() {
                if transition.program_id().name().to_string() == *PROGRAM_ID {
                    info!("process transition {}, function name: {}", transition.id(), transition.function_name().to_string());
                    match transition.function_name().to_string() {
                        name if name == *REGISTER => register(&db_trans, &block, &transaction, transition).await?,
                        name if name == *REGISTER_TLD => register_tld(&db_trans, &block, &transaction, transition).await?,
                        name if name == *REGISTER_PRIVATE => register(&db_trans, &block, &transaction, transition).await?,
                        name if name == *REGISTER_PUBLIC => register(&db_trans, &block, &transaction, transition).await?,
                        name if name == *TRANSFER_PRIVATE_TO_PUBLIC => transfer_private_to_public(&db_trans, &block, &transaction, transition).await?,
                        name if name == *TRANSFER_PUBLIC_TO_PRIVATE => transfer_public_to_private(&db_trans, &block, &transaction, transition).await?,
                        name if name == *TRANSFER_FROM_PUBLIC => transfer_from_public(&db_trans, &block, &transaction, transition).await?,
                        name if name == *TRANSFER_PUBLIC => transfer_public(&db_trans, &block, &transaction, transition).await?,
                        name if name == *TRANSFER_PRIVATE => transfer_private(&db_trans, &block, &transaction, transition).await?,
                        name if name == *SET_PRIMARY_NAME => set_primary_name(&db_trans, &block, &transaction, transition).await?,
                        name if name == *UNSET_PRIMARY_NAME => unset_primary_name(&db_trans, &block, &transaction, transition).await?,
                        name if name == *SET_RESOLVER => set_resolver(&db_trans, &block, &transaction, transition).await?,
                        name if name == *BURN => burn(&db_trans, &block, &transaction, transition).await?,
                        _ => {}
                    }
                }
                else if transition.program_id().name().to_string() == *TRANSFER_PROGRAM_ID {
                    info!("process transition {}, function name: {}", transition.id(), transition.function_name().to_string());
                    match transition.function_name().to_string() {
                        name if name == *TRANSFER_CREDITS => transfer_credits(&db_trans, &block, &transaction, transition).await?,
                        name if name == *TRANSFER_CREDITS_PUBLIC => transfer_credits(&db_trans, &block, &transaction, transition).await?,
                        name if name == *TRANSFER_TOKEN => transfer_token(&db_trans, &block, &transaction, transition).await?,
                        name if name == *TRANSFER_TOKEN_PUBLIC => transfer_token(&db_trans, &block, &transaction, transition).await?,
                        name if name == *CLAIM_CREDITS_PUBLIC => claim_credits(&db_trans, &block, &transaction, transition).await?,
                        name if name == *CLAIM_CREDITS_PRIVATE => claim_credits(&db_trans, &block, &transaction, transition).await?,
                        name if name == *CLAIM_CREDITS_AS_SIGNER => claim_credits(&db_trans, &block, &transaction, transition).await?,
                        name if name == *CLAIM_TOKEN_PUBLIC => claim_token(&db_trans, &block, &transaction, transition).await?,
                        name if name == *CLAIM_TOKEN_PRIVATE => claim_token(&db_trans, &block, &transaction, transition).await?,
                        name if name == *CLAIM_TOKEN_AS_SIGNER => claim_token(&db_trans, &block, &transaction, transition).await?,
                        _ => {}
                    }
                }
                else if transition.program_id().name().to_string() == *RECORD_PROGRAM_ID {
                    info!("process transition {}, function name: {}", transition.id(), transition.function_name().to_string());
                    match transition.function_name().to_string() {
                        name if name == *SET_RESOLVER_RECORD => set_resolver_record(&db_trans, &block, &transaction, transition).await?,
                        name if name == *UNSET_RESOLVER_RECORD => unset_resolver_record(&db_trans, &block, &transaction, transition).await?,
                        name if name == *SET_RESOLVER_RECORD_PUBLIC => set_resolver_record(&db_trans, &block, &transaction, transition).await?,
                        name if name == *UNSET_RESOLVER_RECORD_PUBLIC => unset_resolver_record(&db_trans, &block, &transaction, transition).await?,
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}

/**
process all register transition
 **/
async fn register<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).ok_or("No name_hash_arg")?;
        let name_arg = args.get(1).ok_or("No name_arg")?;
        let parent_arg = args.get(2).ok_or("No parent_arg")?;
        let resolver_arg = args.get(3).ok_or("No resolver_arg")?;

        let name_hash: String = parse_field(name_hash_arg)?;
        let name = parse_str_4u128(name_arg)?;
        let parent: String = parse_field(parent_arg)?;
        let resolver = parse_str_u128(resolver_arg)?;
        let transfer_key = utils::get_name_hash_transfer_key(&name_hash)?.to_string();
        let mut full_name = name.clone();

        let query = "SELECT full_name FROM ans_name WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await?;
        let rows = db_trans.query(&query, &[&parent]).await?;
        if !rows.is_empty() {
            let parent_full_name: String = rows.get(0).unwrap().get(0);
            full_name = name.clone() + &".".to_string() + &parent_full_name;
        }

        let name_field = utils::parse_name_field(&full_name)?.to_string();

        db_trans.execute("INSERT INTO ans_name (name_hash, name_field, transfer_key, name, parent, resolver, full_name, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &name_field, &transfer_key, &name, &parent, &resolver, &full_name, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;

        info!("register: {} {} {} {} {}", name, parent, name_hash, full_name, resolver);
        Ok(())
    } else {
        error!("register: Error in {} | {}", block.height(), transaction.id());
        Err("register: missing future output".into())
    }
}

async fn register_tld<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        // let hash_caller_arg = args.get(0).unwrap();
        let registrar_arg = args.get(1).ok_or("No registrar_arg")?;
        let name_hash_arg = args.get(2).ok_or("No name_hash_arg")?;
        let name_arg = args.get(3).ok_or("No name_arg")?;

        let registrar: String = parse_address(registrar_arg)?;
        let name_hash: String = parse_field(name_hash_arg)?;
        let name = parse_str_name_struct(name_arg)?;
        let transfer_key = utils::get_name_hash_transfer_key(&name_hash)?.to_string();
        let name_field = utils::parse_name_field(&name)?.to_string();

        db_trans.execute("INSERT INTO ans_name (name_hash, name_field, transfer_key, name, parent, resolver, full_name, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &name_field, &transfer_key, &name, &"0field".to_string(), &"".to_string(), &name, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;
        db_trans.execute("INSERT INTO ans_nft_owner (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &registrar, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;

        info!("register_tld {} {} {}", name_hash, name, registrar);
        Ok(())
    } else {
        error!("register_tld: Error in {} | {}", block.height(), transaction.id());
        Err("register_tld: missing future output".into())
    }
}

async fn transfer_private_to_public<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(0).ok_or("No owner_arg")?;
        let name_hash_arg = args.get(1).ok_or("No name_hash_arg")?;

        let owner: String = parse_address(owner_arg)?;
        let name_hash: String = parse_field(name_hash_arg)?;

        db_trans.execute("INSERT INTO ans_nft_owner (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &owner, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;

        info!("convert_private_to_public {} {}", name_hash, owner);
        Ok(())
    } else {
        error!("convert_private_to_public: Error in {} | {}", block.height(), transaction.id());
        Err("transfer_private_to_public: missing future output".into())
    }
}

async fn transfer_public_to_private<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(0).ok_or("No owner_arg")?;
        let name_hash_arg = args.get(1).ok_or("No name_hash_arg")?;

        let owner: String = parse_address(owner_arg)?;
        let name_hash: String = parse_field(name_hash_arg)?;

        db_trans.execute("DELETE from ans_nft_owner WHERE name_hash=$1", &[&name_hash]).await?;
        version_update(&db_trans, &block, &transaction, &transition, &name_hash, &owner).await;

        info!("convert_public_to_private {} {}", name_hash, owner);
        Ok(())
    } else {
        error!("convert_public_to_private: Error in {} | {}", block.height(), transaction.id());
        Err("transfer_public_to_private: missing future output".into())
    }
}

async fn transfer_from_public<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(1).ok_or("No owner_arg")?;
        let new_owner_arg = args.get(2).ok_or("No new_owner_arg")?;
        let name_hash_arg = args.get(3).ok_or("No name_hash_arg")?;

        let owner: String = parse_address(owner_arg)?;
        let new_owner: String = parse_address(new_owner_arg)?;
        let name_hash: String = parse_field(name_hash_arg)?;

        db_trans.execute("INSERT INTO ans_nft_owner (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET address = $2, block_height=$3, transaction_id=$4, transition_id=$5 ",
                         &[&name_hash, &new_owner, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;

        version_update(&db_trans, &block, &transaction, &transition, &name_hash, &owner).await;

        info!("transfer_from_public {} {} to {}", name_hash, owner, new_owner);
        Ok(())
    } else {
        error!("transfer_from_public: Error in {} | {}", block.height(), transaction.id());
        Err("transfer_from_public: missing future output".into())
    }
}

async fn transfer_public<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let receiver_arg = args.get(0).ok_or("No receiver_arg")?;
        let name_hash_arg = args.get(1).ok_or("No name_hash_arg")?;
        let caller_arg = args.get(2).ok_or("No caller_arg")?;

        let receiver: String = parse_address(receiver_arg)?;
        let name_hash: String = parse_field(name_hash_arg)?;
        let caller: String = parse_address(caller_arg)?;

        let query = "SELECT address FROM ans_nft_owner WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await?;
        let rows = db_trans.query(&query, &[&name_hash]).await?;
        let owner:String = rows.get(0).unwrap().get(0);

        db_trans.execute("INSERT INTO ans_nft_owner (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET address = $2, block_height=$3, transaction_id=$4, transition_id=$5 ",
                         &[&name_hash, &receiver, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;

        version_update(&db_trans, &block, &transaction, &transition, &name_hash, &owner).await;

        info!(">> transfer_public {} {} caller {}", name_hash, owner, caller);
        Ok(())
    } else {
        error!(">> transfer_public: Error in {} | {}", block.height(), transaction.id());
        Err("transfer_public: missing future output".into())
    }
}

async fn transfer_private<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).ok_or("No name_hash_arg")?;
        let name_hash: String = parse_field(name_hash_arg)?;
        version_update(&db_trans, &block, &transaction, &transition, &name_hash, "").await;
        info!("transfer_private {}", name_hash);
        Ok(())
    } else {
        error!("transfer_private: Error in {} | {}", block.height(), transaction.id());
        Err("transfer_private: missing future output".into())
    }
}

async fn set_primary_name<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).ok_or("No name_hash_arg")?;
        let owner_arg = args.get(1).ok_or("No owner_arg")?;
        let name_hash: String = parse_field(name_hash_arg)?;
        let owner: String = parse_address(owner_arg)?;
        db_trans.execute("INSERT INTO ans_primary_name (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (address) DO UPDATE SET name_hash = $1, block_height=$3, transaction_id=$4, transition_id=$5 ",
                         &[&name_hash, &owner, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;
        info!("set_primary_name {} {}", name_hash, owner);
        Ok(())
    } else {
        error!("set_primary_name: Error in {} | {}", block.height(), transaction.id());
        Err("set_primary_name: missing future output".into())
    }
}

async fn unset_primary_name<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(0).ok_or("No owner_arg")?;
        let owner: String = parse_address(owner_arg)?;
        db_trans.execute("DELETE from ans_primary_name WHERE address=$1", &[&owner]).await?;
        info!("unset_primary_name {}", owner);
        Ok(())
    } else {
        error!("unset_primary_name: Error in {} | {}", block.height(), transaction.id());
        Err("unset_primary_name: missing future output".into())
    }
}

async fn set_resolver<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).ok_or("No name_hash_arg")?;
        let owner_arg = args.get(1).ok_or("No owner_arg")?;
        let resolver_arg = args.get(2).ok_or("No resolver_arg")?;
        let name_hash: String = parse_field(name_hash_arg)?;
        let owner: String = parse_address(owner_arg)?;
        let resolver = parse_str_field(resolver_arg, true)?;
        db_trans.execute("UPDATE ans_name set resolver=$1  WHERE name_hash=$2 ",
                         &[&resolver, &name_hash]
        ).await?;

        info!("set_resolver {} {}", name_hash, owner);
        Ok(())
    } else {
        error!("set_resolver: Error in {} | {}", block.height(), transaction.id());
        Err("set_resolver: missing future output".into())
    }
}

async fn set_resolver_record<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).ok_or("No name_hash_arg")?;
        let category_arg = args.get(1).ok_or("No category_arg")?;
        let content_arg = args.get(2).ok_or("No content_arg")?;
        let name_hash: String = parse_field(name_hash_arg)?;
        let category: String = parse_str_u128(category_arg)?;
        let content = parse_str_8u128(content_arg)?;
        let mut version = 1;
        let query = "SELECT version FROM ans_name_version WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await?;
        let rows = db_trans.query(&query, &[&name_hash]).await?;
        if !rows.is_empty() {
            version = rows.get(0).unwrap().get(0);
        }
        db_trans.execute("INSERT INTO ans_resolver (name_hash, category, version, name, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5, $6, $7) ON CONFLICT (name_hash, category, version) DO UPDATE SET name=$4, block_height=$5, transaction_id=$6, transition_id=$7",
                         &[&name_hash, &category, &version, &content, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;
        info!("set_resolver_record: {} {} {} {}", name_hash, category, content, version);
        Ok(())
    } else {
        error!("set_resolver_record: Error in {} | {}", block.height(), transaction.id());
        Err("set_resolver_record: missing future output".into())
    }
}

async fn unset_resolver_record<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).ok_or("No name_hash_arg")?;
        let category_arg = args.get(1).ok_or("No category_arg")?;
        let name_hash: String = parse_field(name_hash_arg)?;
        let category: String = parse_str_u128(category_arg)?;
        let mut version = 1;
        let query = "SELECT version FROM ans_name_version WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await?;
        let rows = db_trans.query(&query, &[&name_hash]).await?;
        if !rows.is_empty() {
            version = rows.get(0).unwrap().get(0);
        }
        db_trans.execute("DELETE from ans_resolver where name_hash=$1 and category=$2 and version=$3 ",
                         &[&name_hash, &category, &version]
        ).await?;
        info!("unset_resolver_record: {} {} {}", name_hash, category, version);
        Ok(())
    } else {
        error!("unset_resolver_record: Error in {} | {}", block.height(), transaction.id());
        Err("unset_resolver_record: missing future output".into())
    }
}

async fn burn<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).ok_or("No name_hash_arg")?;
        let name_hash: String = parse_field(name_hash_arg)?;
        db_trans.execute("DELETE from ans_name WHERE name_hash=$1", &[&name_hash]).await?;
        version_update(&db_trans, &block, &transaction, &transition, &name_hash, "").await;
        info!("burn: {} in {}|{}", name_hash, block.height(), transaction.id());
        Ok(())
    } else {
        error!("burn: Error  in {} | {}", block.height(), transaction.id());
        Err("burn: missing future output".into())
    }
}

async fn transfer_credits<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let transfer_key_arg = args.get(args.len()  - 2).ok_or("No transfer_key_arg")?;
        let amount_arg = args.get(args.len() - 1).ok_or("No amount_arg")?;
        let transfer_key: String = parse_field(transfer_key_arg)?;
        let amount: u64 = parse_u64(amount_arg)?;
        db_trans.execute("INSERT INTO domain_credits (transfer_key, amount, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (transfer_key) DO UPDATE SET amount = domain_credits.amount + $2, block_height=$3, transaction_id=$4, transition_id=$5",
                         &[&transfer_key, &Decimal::from_u64(amount), &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;
        info!("transfer_credits: {} {} in {}|{}", transfer_key, amount, block.height(), transaction.id());
        Ok(())
    } else {
        error!("transfer_credits: Error  in {} | {}", block.height(), transaction.id());
        Err("transfer_credits: missing future output".into())
    }
}

async fn claim_credits<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let transfer_key_arg = args.get(args.len()  - 2).ok_or("No transfer_key_arg")?;
        let amount_arg = args.get(args.len() - 1).ok_or("No amount_arg")?;
        let transfer_key: String = parse_field(transfer_key_arg)?;
        let amount: u64 = parse_u64(amount_arg)?;
        db_trans.execute("UPDATE domain_credits SET amount = domain_credits.amount - $2, block_height=$3, transaction_id=$4, transition_id=$5 where transfer_key=$1",
                         &[&transfer_key, &Decimal::from_u64(amount), &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;
        info!("transfer_credits: {} {} in {}|{}", transfer_key, amount, block.height(), transaction.id());
        Ok(())
    } else {
        error!("transfer_credits: Error  in {} | {}", block.height(), transaction.id());
        Err("claim_credits: missing future output".into())
    }
}

async fn transfer_token<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let transfer_key_arg = args.get(args.len()  - 2).ok_or("No transfer_key_arg")?;
        let amount_arg = args.get(args.len() - 1).ok_or("No amount_arg")?;
        let transfer_key: String = parse_field(transfer_key_arg)?;
        let amount: u128 = parse_u128(amount_arg)?;
        db_trans.execute("INSERT INTO domain_credits (transfer_key, amount, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (transfer_key) DO UPDATE SET amount = domain_credits.amount + $2, block_height=$3, transaction_id=$4, transition_id=$5",
                         &[&transfer_key, &Decimal::from_u128(amount), &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;
        info!("transfer_credits: {} {} in {}|{}", transfer_key, amount, block.height(), transaction.id());
        Ok(())
    } else {
        error!("transfer_credits: Error  in {} | {}", block.height(), transaction.id());
        Err("transfer_token: missing future output".into())
    }
}

async fn claim_token<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).ok_or("No output found")?;
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let transfer_key_arg = args.get(args.len()  - 2).ok_or("No transfer_key_arg")?;
        let amount_arg = args.get(args.len() - 1).ok_or("No amount_arg")?;
        let transfer_key: String = parse_field(transfer_key_arg)?;
        let amount: u128 = parse_u128(amount_arg)?;
        db_trans.execute("UPDATE domain_credits SET amount = domain_credits.amount - $2, block_height=$3, transaction_id=$4, transition_id=$5 where transfer_key=$1",
                         &[&transfer_key, &Decimal::from_u128(amount), &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await?;
        info!("transfer_credits: {} {} in {}|{}", transfer_key, amount, block.height(), transaction.id());
        Ok(())
    } else {
        error!("transfer_credits: Error  in {} | {}", block.height(), transaction.id());
        Err("claim_token: missing future output".into())
    }
}

async fn version_update<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>, name_hash: &str, owner: &str) {
    let version = 2;
    db_trans.execute("INSERT INTO ans_name_version (name_hash, version, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET version = ans_name_version.version + 1, block_height=$3, transaction_id=$4, transition_id=$5",
                     &[&name_hash, &version, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
    ).await.unwrap();
    db_trans.execute("DELETE from ans_primary_name WHERE name_hash=$1 AND address=$2", &[&name_hash, &owner]).await.unwrap();
}

fn parse_str_name_struct<N: Network>(name_arg: &Argument<N>) -> Result<String, String> {
    let name_bytes = Argument::to_bytes_le(name_arg).unwrap();
    let mut name: [u8; 64] = [0; 64];

    name[0..16].copy_from_slice(&name_bytes[20..36]);
    name[16..32].copy_from_slice(&name_bytes[41..57]);
    name[32..48].copy_from_slice(&name_bytes[62..78]);
    name[48..64].copy_from_slice(&name_bytes[83..99]);

    Ok(std::str::from_utf8(&name).unwrap().trim_matches('\0').to_string())
}

// parse argument
fn parse_str_4u128<N: Network>(name_arg: &Argument<N>) -> Result<String, String> {
    let name_bytes = Argument::to_bytes_le(name_arg).unwrap();
    let mut name: [u8; 64] = [0; 64];

    name[0..16].copy_from_slice(&name_bytes[11..27]);
    name[16..32].copy_from_slice(&name_bytes[32..48]);
    name[32..48].copy_from_slice(&name_bytes[53..69]);
    name[48..64].copy_from_slice(&name_bytes[74..90]);

    Ok(std::str::from_utf8(&name).unwrap().trim_matches('\0').to_string())
}

fn parse_str_8u128<N: Network>(name_arg: &Argument<N>) -> Result<String, String> {
    let name_bytes = Argument::to_bytes_le(name_arg).unwrap();
    let mut name: [u8; 128] = [0; 128];

    name[0..16].copy_from_slice(&name_bytes[11..27]);
    name[16..32].copy_from_slice(&name_bytes[32..48]);
    name[32..48].copy_from_slice(&name_bytes[53..69]);
    name[48..64].copy_from_slice(&name_bytes[74..90]);
    name[64..80].copy_from_slice(&name_bytes[95..111]);
    name[80..96].copy_from_slice(&name_bytes[116..132]);
    name[96..112].copy_from_slice(&name_bytes[137..153]);
    name[112..128].copy_from_slice(&name_bytes[158..174]);

    Ok(std::str::from_utf8(&name).unwrap().trim_matches('\0').to_string())
}

fn parse_str_u128<N: Network>(name_arg: &Argument<N>) -> Result<String, String> {
    let name_bytes = Argument::to_bytes_le(name_arg).unwrap();
    let mut name: [u8; 16] = [0; 16];

    name[0..16].copy_from_slice(&name_bytes[4..20]);
    Ok(std::str::from_utf8(&name).unwrap().trim_matches('\0').to_string())
}

fn parse_str_field<N: Network>(name_arg: &Argument<N>, reverse: bool) -> Result<String, String> {
    let name_bytes = Argument::to_bytes_le(name_arg).unwrap();
    let mut name: [u8; 32] = [0; 32];

    name[0..32].copy_from_slice(&name_bytes[4..36]);
    if reverse {
        name.reverse();
    }
    Ok(std::str::from_utf8(&name).unwrap().trim_matches('\0').to_string())
}

fn parse_field<N: Network>(field_arg: &Argument<N>) -> Result<String, String> {
    let field_arg_bytes = Argument::to_bytes_le(field_arg).unwrap();

    if field_arg_bytes.len() >= 32 {
        let last_32: &[u8] = &field_arg_bytes[field_arg_bytes.len() - 32..];
        Ok(format!("{}", Field::<N>::from_bytes_le(last_32).unwrap()))
    } else {
        Err("e".to_string())
    }
}

fn parse_address<N: Network>(address_arg: &Argument<N>) -> Result<String, String> {
    let address_arg_bytes = Argument::to_bytes_le(address_arg).unwrap();

    if address_arg_bytes.len() >= 32 {
        let last_32: &[u8] = &address_arg_bytes[address_arg_bytes.len() - 32..];
        Ok(format!("{}", Address::<N>::from_bytes_le(last_32).unwrap()))
    } else {
        Err("e".to_string())
    }
}

fn parse_u64<N: Network>(u64_arg: &Argument<N>) -> Result<u64, String> {
    let u64_arg_bytes = Argument::to_bytes_le(u64_arg).unwrap();

    if u64_arg_bytes.len() >= 8 {
        let last_8: &[u8] = &u64_arg_bytes[u64_arg_bytes.len() - 8..];
        Ok(u64::from_le_bytes(last_8.try_into().unwrap()))
    } else {
        Err("e".to_string())
    }
}

fn parse_u128<N: Network>(u128_arg: &Argument<N>) -> Result<u128, String> {
    let u128_arg_bytes = Argument::to_bytes_le(u128_arg).unwrap();

    if u128_arg_bytes.len() >= 16 {
        let last_16: &[u8] = &u128_arg_bytes[u128_arg_bytes.len() - 16..];
        Ok(u128::from_le_bytes(last_16.try_into().unwrap()))
    } else {
        Err("e".to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use snarkvm_console_network::MainnetV0;
    use snarkvm_console_program::Future;
    use snarkvm_ledger_block::Block;
    use tracing::{error, info};
    use crate::indexer::{extract_block_basic_info, index_data, parse_str_field, parse_u128, preprocess_json, process_block_data};

    #[test]
    fn test_parse_plaintext() {
        let fu = Future::<MainnetV0>::from_str(
            "{ program_id: test.aleo, function_name: test, arguments: [ 418262508645field, 123u128 ] }",
        ).unwrap();
        let f = fu.arguments().get(0).unwrap();
        let s = parse_str_field(f, true).unwrap();
        assert!(s.eq("abcde"));
        let f = fu.arguments().get(1).unwrap();
        let u = parse_u128(f).unwrap();
        assert!(u == 123u128)
    }

    #[test]
    fn test_parse() {
        // read the JSON data from file/test_block_parse.json file
        let json_data = std::fs::read_to_string("src/file/test_block_parse.json").unwrap();

        match serde_json::from_str::<Vec<serde_json::Value>>(&json_data) {
            Ok(blocks_json) => {
                for (i, block_value) in blocks_json.iter().enumerate() {
                    let block_json = block_value.to_string();
                    match extract_block_basic_info(&block_json) {
                        Ok(basic_info) => {
                            info!("Block {}: Height: {}, Hash: {}, Timestamp: {}, Has Relevant Programs: {}",
                                  i, basic_info.height, basic_info.hash, basic_info.timestamp, basic_info.has_relevant_programs);
                            if basic_info.has_relevant_programs {
                                match serde_json::from_str::<Block<MainnetV0>>(&block_json) {
                                    Ok(block) => {
                                        assert!(true);
                                    },
                                    Err(e) => assert!(false, "Failed to parse block {}: {}", i, e),
                                }
                            } else {
                                info!("Block {} contains no relevant programs, skipping detailed parsing", basic_info.height);
                            }
                        },
                        Err(e) => {
                            assert!(false, "Failed to process block {}: {}", i, block_json);
                        }
                    }
                }
            },
            Err(e) => assert!(false, "Failed to process blocks： {}, json head: {}", e, &json_data.chars().take(20).collect::<String>()),
        }
    }
}
