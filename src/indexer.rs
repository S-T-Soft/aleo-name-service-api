use std::{env, fmt};
use std::cmp::max;
use std::error::Error;
use std::str::FromStr;
use std::time::Duration;
use lazy_static::lazy_static;
use regex::Regex;
use serde_json::Value;
use tokio::time::sleep;
use snarkvm_console_network::{MainnetV0, Network};
use snarkvm_ledger_block::{Block, Transaction};
use snarkvm_console_network::prelude::ToBytes;
use snarkvm_console_program::{Field, Address, Argument, FromBytes};
use snarkvm_ledger_block::{Transition};
use tokio_postgres::NoTls;
use tracing::{error, info, warn};
use crate::{client, utils};

static MAX_BLOCK_RANGE: u32 = 50;
const CDN_ENDPOINT: &str = "https://s3.us-west-1.amazonaws.com/testnet.blocks/phase3";

#[derive(Debug)]
struct IndexError(Box<dyn Error>);

lazy_static! {
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
    static ref CLAIM_CREDITS_PUBLIC: &'static str = "claim_credits_public";
    static ref CLAIM_CREDITS_PRIVATE: &'static str = "claim_credits_private";

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

    loop {
        let block_number = get_next_block_number(latest_height).await.unwrap_or_else(|e| {
            eprintln!("Error fetching next block number: {}", e);
            0
        });

        if block_number > 0 {
            if (latest_height - block_number) > 10 {
                match client::get_blocks(block_number as u32, block_number as u32 + 10).await {
                    Ok(response) => {
                        let response = preprocess_json(&response);
                        match serde_json::from_str::<Vec<Block<N>>>(&response) {
                            Ok(blocks) => {
                                for data in blocks {
                                    index_data(&data).await;
                                }
                            },
                            Err(e) => eprintln!("Error parse response: {}", e)
                        }
                    },
                    Err(e) => eprintln!("Error fetching data: {}", e),
                }

            } else {
                match client::get_block(block_number as u32).await {
                    Ok(response) => {
                        let response = preprocess_json(&response);
                        match serde_json::from_str::<Block<N>>(&response) {
                            Ok(data) => index_data(&data).await,
                            Err(e) => eprintln!("Error fetching data: {}", e),
                        }
                    },
                    Err(e) => eprintln!("Error fetching data: {}", e),
                }
            }

            sleep(Duration::from_micros(50)).await;
        } else {
            sleep(Duration::from_secs(5)).await;
        }

        if block_number > latest_height {
            latest_height = get_latest_height().await as i64;
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
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn get_next_block_number(init_latest_height: i64) -> Result<i64, Box<dyn Error>> {
    let mut local_latest_height = *ANS_BLOCK_HEIGHT_START;
    let db_client = DB_POOL.get().await?;
    let db_schema = env::var("DB_SCHEMA").unwrap_or_else(|_| "ansb".to_string());
    db_client.execute(format!("SET search_path TO {db_schema}").as_str(), &[]).await.unwrap();

    let query = "select height from block order by height desc limit 1";
    let query = db_client.prepare(&query).await.unwrap();
    let rows = db_client.query(&query, &[]).await?;
    if !rows.is_empty() {
        local_latest_height = max(rows.get(0).unwrap().get(0), local_latest_height);
    }

    let mut latest_height= init_latest_height;
    if local_latest_height >= init_latest_height {
        latest_height = get_latest_height().await as i64;
    }

    info!("Latest height: {}", latest_height);

    let height = if latest_height as i64 > local_latest_height {
        local_latest_height + 1
    } else {
        0
    };
    Ok(height)
}

async fn index_data<N: Network>(block: &Block<N>) {
    info!("Process block {} on {}", block.height(), block.timestamp());
    let mut db_client = DB_POOL.get().await.unwrap();
    let db_schema = env::var("DB_SCHEMA").unwrap_or_else(|_| "ansb".to_string());
    db_client.execute(format!("SET search_path TO {db_schema}").as_str(), &[]).await.unwrap();
    let db_trans = db_client.transaction().await.unwrap();

    db_trans.execute("INSERT INTO block (height, block_hash, previous_hash, timestamp) VALUES ($1, $2,$3, $4) ON CONFLICT (height) DO NOTHING",
                     &[&(block.height() as i64), &block.hash().to_string(), &block.previous_hash().to_string(), &block.timestamp()]).await.unwrap();

    for transaction in block.transactions().clone().into_iter() {
        if transaction.is_accepted() {
            for transition in transaction.transitions() {
                if transition.program_id().name().to_string() == *PROGRAM_ID {
                    info!("process transition {}, function name: {}", transition.id(), transition.function_name().to_string());
                    match transition.function_name().to_string() {
                        name if name == *REGISTER => register(&db_trans, &block, &transaction, transition).await,
                        name if name == *REGISTER_TLD => register_tld(&db_trans, &block, &transaction, transition).await,
                        name if name == *REGISTER_PRIVATE => register(&db_trans, &block, &transaction, transition).await,
                        name if name == *REGISTER_PUBLIC => register(&db_trans, &block, &transaction, transition).await,
                        name if name == *TRANSFER_PRIVATE_TO_PUBLIC => transfer_private_to_public(&db_trans, &block, &transaction, transition).await,
                        name if name == *TRANSFER_PUBLIC_TO_PRIVATE => transfer_public_to_private(&db_trans, &block, &transaction, transition).await,
                        name if name == *TRANSFER_FROM_PUBLIC => transfer_from_public(&db_trans, &block, &transaction, transition).await,
                        name if name == *TRANSFER_PUBLIC => transfer_public(&db_trans, &block, &transaction, transition).await,
                        name if name == *TRANSFER_PRIVATE => transfer_private(&db_trans, &block, &transaction, transition).await,
                        name if name == *SET_PRIMARY_NAME => set_primary_name(&db_trans, &block, &transaction, transition).await,
                        name if name == *UNSET_PRIMARY_NAME => unset_primary_name(&db_trans, &block, &transaction, transition).await,
                        name if name == *SET_RESOLVER => set_resolver(&db_trans, &block, &transaction, transition).await,
                        name if name == *BURN => burn(&db_trans, &block, &transaction, transition).await,
                        _ => {}
                    }
                }
                else if transition.program_id().name().to_string() == *TRANSFER_PROGRAM_ID {
                    info!("process transition {}, function name: {}", transition.id(), transition.function_name().to_string());
                    match transition.function_name().to_string() {
                        name if name == *TRANSFER_CREDITS => transfer_credits(&db_trans, &block, &transaction, transition).await,
                        name if name == *CLAIM_CREDITS_PUBLIC => claim_credits(&db_trans, &block, &transaction, transition).await,
                        name if name == *CLAIM_CREDITS_PRIVATE => claim_credits(&db_trans, &block, &transaction, transition).await,
                        _ => {}
                    }
                }
                else if transition.program_id().name().to_string() == *RECORD_PROGRAM_ID {
                    info!("process transition {}, function name: {}", transition.id(), transition.function_name().to_string());
                    match transition.function_name().to_string() {
                        name if name == *SET_RESOLVER_RECORD => set_resolver_record(&db_trans, &block, &transaction, transition).await,
                        name if name == *UNSET_RESOLVER_RECORD => unset_resolver_record(&db_trans, &block, &transaction, transition).await,
                        name if name == *SET_RESOLVER_RECORD_PUBLIC => set_resolver_record(&db_trans, &block, &transaction, transition).await,
                        name if name == *UNSET_RESOLVER_RECORD_PUBLIC => unset_resolver_record(&db_trans, &block, &transaction, transition).await,
                        _ => {}
                    }
                }
            }
        }
    }

    db_trans.commit().await.unwrap()
}

/**
process all register transition
 **/
async fn register<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).unwrap();
        let name_arg = args.get(1).unwrap();
        let parent_arg = args.get(2).unwrap();
        let resolver_arg = args.get(3).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let name = parse_str_4u128(name_arg).unwrap();
        let parent: String = parse_field(parent_arg).unwrap();
        let resolver = parse_str_u128(resolver_arg).unwrap();
        let transfer_key = utils::get_name_hash_transfer_key(&name_hash).unwrap().to_string();
        let mut full_name = name.clone();

        let query = "SELECT full_name FROM ans_name WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await.unwrap();
        let rows = db_trans.query(&query, &[&parent]).await.unwrap();
        if !rows.is_empty() {
            let parent_full_name = rows.get(0).unwrap().get(0);
            full_name = name.clone() + &".".to_string() + parent_full_name;
        }

        let name_field = utils::parse_name_field(&full_name).unwrap().to_string();

        db_trans.execute("INSERT INTO ans_name (name_hash, name_field, transfer_key, name, parent, resolver, full_name, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &name_field, &transfer_key, &name, &parent, &resolver, &full_name, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        info!("register: {} {} {} {} {}", name, parent, name_hash, full_name, resolver)
    } else {
        error!("register: Error in {} | {}", block.height(), transaction.id())
    };

}

async fn register_tld<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        // let hash_caller_arg = args.get(0).unwrap();
        let registrar_arg = args.get(1).unwrap();
        let name_hash_arg = args.get(2).unwrap();
        let name_arg = args.get(3).unwrap();

        let registrar: String = parse_address(registrar_arg).unwrap();
        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let name = parse_str_name_struct(name_arg).unwrap();
        let transfer_key = utils::get_name_hash_transfer_key(&name_hash).unwrap().to_string();

        let name_field = utils::parse_name_field(&name).unwrap().to_string();

        db_trans.execute("INSERT INTO ans_name (name_hash, name_field, transfer_key, name, parent, resolver, full_name, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &name_field, &transfer_key, &name, &"0field".to_string(), &"".to_string(), &name, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();
        db_trans.execute("INSERT INTO ans_nft_owner (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &registrar, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        info!("register_tld {} {} {}", name_hash, name, registrar)
    } else {
        error!("register_tld: Error in {} | {}", block.height(), transaction.id())
    };

}

async fn transfer_private_to_public<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(0).unwrap();
        let name_hash_arg = args.get(1).unwrap();

        let owner: String = parse_address(owner_arg).unwrap();
        let name_hash: String = parse_field(name_hash_arg).unwrap();

        db_trans.execute("INSERT INTO ans_nft_owner (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &owner, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        info!("convert_private_to_public {} {}", name_hash, owner)
    } else {
        error!("convert_private_to_public: Error in {} | {}", block.height(), transaction.id())
    };
}

async fn transfer_public_to_private<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(0).unwrap();
        let name_hash_arg = args.get(1).unwrap();

        let owner: String = parse_address(owner_arg).unwrap();
        let name_hash: String = parse_field(name_hash_arg).unwrap();

        db_trans.execute("DELETE from ans_nft_owner WHERE name_hash=$1", &[&name_hash]).await.unwrap();

        version_update(&db_trans, &block, &transaction, &transition, &name_hash, &owner).await;

        info!("convert_public_to_private {} {}", name_hash, owner)
    } else {
        error!("convert_public_to_private: Error in {} | {}", block.height(), transaction.id())
    };
}

async fn transfer_from_public<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(1).unwrap();
        let new_owner_arg = args.get(2).unwrap();
        let name_hash_arg = args.get(3).unwrap();

        let owner: String = parse_address(owner_arg).unwrap();
        let new_owner: String = parse_address(new_owner_arg).unwrap();
        let name_hash: String = parse_field(name_hash_arg).unwrap();

        db_trans.execute("INSERT INTO ans_nft_owner (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET address = $2, block_height=$3, transaction_id=$4, transition_id=$5 ",
                         &[&name_hash, &new_owner, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        version_update(&db_trans, &block, &transaction, &transition, &name_hash, &owner).await;

        info!("transfer_from_public {} {} to {}", name_hash, owner, new_owner)
    } else {
        error!("transfer_from_public: Error in {} | {}", block.height(), transaction.id())
    };
}

async fn transfer_public<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let receiver_arg = args.get(0).unwrap();
        let name_hash_arg = args.get(1).unwrap();
        let caller_arg = args.get(2).unwrap();

        let receiver: String = parse_address(receiver_arg).unwrap();
        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let caller: String = parse_address(caller_arg).unwrap();

        let query = "SELECT address FROM ans_nft_owner WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await.unwrap();
        let rows = db_trans.query(&query, &[&name_hash]).await.unwrap();

        let owner:String = rows.get(0).unwrap().get(0);


        db_trans.execute("INSERT INTO ans_nft_owner (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET address = $2, block_height=$3, transaction_id=$4, transition_id=$5 ",
                         &[&name_hash, &receiver, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        version_update(&db_trans, &block, &transaction, &transition, &name_hash, &owner).await;

        info!(">> transfer_public {} {} caller {}", name_hash, owner, caller)
    } else {
        error!(">> transfer_public: Error in {} | {}", block.height(), transaction.id())
    };
}

async fn transfer_private<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();

        version_update(&db_trans, &block, &transaction, &transition, &name_hash, "").await;

        info!("transfer_private {}", name_hash)
    } else {
        error!("transfer_private: Error in {} | {}", block.height(), transaction.id())
    };
}

async fn set_primary_name<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();

        let name_hash_arg = args.get(0).unwrap();
        let owner_arg = args.get(1).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let owner: String = parse_address(owner_arg).unwrap();

        db_trans.execute("INSERT INTO ans_primary_name (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (address) DO UPDATE SET address = $2, block_height=$3, transaction_id=$4, transition_id=$5 ",
                         &[&name_hash, &owner, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        info!("set_primary_name {} {}", name_hash, owner)
    } else {
        error!("set_primary_name: Error in {} | {}", block.height(), transaction.id())
    };
}

async fn unset_primary_name<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(0).unwrap();

        let owner: String = parse_address(owner_arg).unwrap();

        db_trans.execute("DELETE from ans_primary_name WHERE address=$1", &[&owner]).await.unwrap();

        info!("unset_primary_name {}", owner)
    } else {
        error!("unset_primary_name: Error in {} | {}", block.height(), transaction.id())
    };
}

async fn set_resolver<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();

        let name_hash_arg = args.get(0).unwrap();
        let owner_arg = args.get(1).unwrap();
        let resolver_arg = args.get(2).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let owner: String = parse_address(owner_arg).unwrap();
        let resolver = parse_str_u128(resolver_arg).unwrap();

        db_trans.execute("UPDATE ans_name set resolver=$1  WHERE name_hash=$2 ",
                         &[&resolver, &name_hash]
        ).await.unwrap();

        info!("set_resolver {} {}", name_hash, owner)
    } else {
        error!("set_resolver: Error in {} | {}", block.height(), transaction.id())
    };
}

async fn set_resolver_record<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).unwrap();
        let category_arg = args.get(1).unwrap();
        let content_arg = args.get(2).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let category: String = parse_str_u128(category_arg).unwrap();
        let content = parse_str_8u128(content_arg).unwrap();

        let mut version = 1;
        let query = "SELECT version FROM ans_name_version WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await.unwrap();
        let rows = db_trans.query(&query, &[&name_hash]).await.unwrap();
        if !rows.is_empty() {
            version = rows.get(0).unwrap().get(0);
        }

        db_trans.execute("INSERT INTO ans_resolver (name_hash, category, version, name, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5, $6, $7) ON CONFLICT (name_hash, category, version) DO UPDATE SET name=$4, block_height=$5, transaction_id=$6, transition_id=$7",
                         &[&name_hash, &category, &version, &content, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        info!("set_resolver_record: {} {} {} {}", name_hash, category, content, version)
    } else {
        error!("set_resolver_record: Error in {} | {}", block.height(), transaction.id())
    };

}

async fn unset_resolver_record<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).unwrap();
        let category_arg = args.get(1).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let category: String = parse_str_u128(category_arg).unwrap();

        let mut version = 1;
        let query = "SELECT version FROM ans_name_version WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await.unwrap();
        let rows = db_trans.query(&query, &[&name_hash]).await.unwrap();
        if !rows.is_empty() {
            version = rows.get(0).unwrap().get(0);
        }

        db_trans.execute("DELETE from ans_resolver where name_hash=$1 and category=$2 and version=$3 ",
                         &[&name_hash, &category, &version]
        ).await.unwrap();

        info!("unset_resolver_record: {} {} {}", name_hash, category, version)
    } else {
        error!("set_resolver_record: Error in {} | {}", block.height(), transaction.id())
    };
}

async fn burn<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();

        db_trans.execute("DELETE from ans_name WHERE name_hash=$1", &[&name_hash]).await.unwrap();
        version_update(&db_trans, &block, &transaction, &transition, &name_hash, "").await;

        info!("burn: {} in {}|{}", name_hash, block.height(), transaction.id())
    } else {
        error!("burn: Error  in {} | {}", block.height(), transaction.id())
    };
}

async fn transfer_credits<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let transfer_key_arg = args.get(args.len()  - 2).unwrap();
        let amount_arg = args.get(args.len() - 1).unwrap();

        let transfer_key: String = parse_field(transfer_key_arg).unwrap();
        let amount: u64 = parse_u64(amount_arg).unwrap();

        db_trans.execute("INSERT INTO domain_credits (transfer_key, amount, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (transfer_key) DO UPDATE SET amount = domain_credits.amount + $2, block_height=$3, transaction_id=$4, transition_id=$5",
                         &[&transfer_key, &(amount as i64), &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        info!("transfer_credits: {} {} in {}|{}", transfer_key, amount, block.height(), transaction.id())
    } else {
        error!("transfer_credits: Error  in {} | {}", block.height(), transaction.id())
    };
}

async fn claim_credits<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let transfer_key_arg = args.get(args.len()  - 2).unwrap();
        let amount_arg = args.get(args.len() - 1).unwrap();

        let transfer_key: String = parse_field(transfer_key_arg).unwrap();
        let amount: u64 = parse_u64(amount_arg).unwrap();

        db_trans.execute("UPDATE domain_credits SET amount = domain_credits.amount - $2, block_height=$3, transaction_id=$4, transition_id=$5 where transfer_key=$1",
                         &[&transfer_key, &(amount as i64), &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        info!("transfer_credits: {} {} in {}|{}", transfer_key, amount, block.height(), transaction.id())
    } else {
        error!("transfer_credits: Error  in {} | {}", block.height(), transaction.id())
    };
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


#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use snarkvm_console_network::MainnetV0;
    use snarkvm_ledger_block::Block;
    use crate::indexer::preprocess_json;

    #[test]
    fn test_parse() {
        let json_data = r#"{"block_hash":"ab1pc83g2v3f9um0n5hy609pmnug4yx8xzfkdjwkuq4nxsxacsdggrqz2tzkj","previous_hash":"ab1psmp9xvezzjpcwtgf054fmynrhdr8wk6xnl5ax0zzszdpsa58vzsxy70lh","header":{"previous_state_root":"sr1temsvkutxtaen6emvl70g7qcyds9jz9xk4past23zzuwql7tkurqstwzkc","transactions_root":"672837308598844098066880811329748098775461185733783417762227232818850764801field","finalize_root":"5394502682720460223866679040676485721036134295189100509612669743366229139675field","ratifications_root":"5064237088514230868286190546905537695675171948002961672627655199114230704612field","solutions_root":"4410372902070477360490981487511752120698115024122669404615578376994481946477field","subdag_root":"4068050930681627086278258928590138481735124269446662492160422591928563779502field","metadata":{"network":0,"round":2414062,"height":1203288,"cumulative_weight":18741903340503531150,"cumulative_proof_target":"0","coinbase_target":35567900716529,"proof_target":8891975179133,"last_coinbase_target":35567900716529,"last_coinbase_timestamp":1729056561,"timestamp":1729056561}},"authority":{"type":"quorum","subdag":{"subdag":{"2414059":[{"batch_header":{"batch_id":"7983661187835694349128436813979024594721029860058172601817395446239722762779field","author":"aleo1wu56llc4eaw08t63944fx7r69f9syneh30zjaykzna8z2wekl5rqem0nad","round":2414059,"timestamp":1729056560,"committee_id":"5607799450220365747440341219014045704854447694770887850255269957972557325543field","transmission_ids":["at1e8qqtxur3fqualq5mt57zteg9h373tks2l0wj989khawkf54zg9qcke9vn.45637030403536908962109923828623389430"],"previous_certificate_ids":["2284631317289414437283648911462221461052731136023682797357212744601785827204field","7640149579724504799178648628142014282292963515543852345888340219233552473839field","608273973952282406013871683776384472590760645231149794142228311881221143744field","1616956867154424767034621481372992803149361173663547198705997152850453182900field","3744988224961072246619260975217039311366463770114882060374297741014468138737field","6577718862910080150119947960491238153462480211714925966379827606930918321355field","7760404408778661184630544612168685397816400938918123502180506201791702398307field","5655265469773645344706463922270349025114067095287648525464831887968482188732field","3138565386551440886402212532946913578141017454891471372452086094929027394110field","6992698151059455795368538672954515356271282981084828778017048705614766607184field","3617885063009428594342996352940933716387125768286697098775479836204381242268field","6266548885740113165122662924059163323861750617859160538684049395496797934588field","4367728412536749617228744745675004829164460810922422673367767573375119492712field"],"signature":"sign1wpz9ux72u49kj2vdsvc9hc358ht4d0jj8j0wv256fpk5gvl5kcqnplk0wx03zr2wpy929jskyje6xr5gsuzj863xrapmqdy4ydj67qr7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq63m0qwe"},"signatures":["sign1qnvlq6qx3vcg72ef87mthppcwhuf0amfaqmr0tzc4uf9n2cz3yq5vgs9r5xjgf3dth3c8ktjzkfrsemr32aw2q32sw6848yea3agqqykdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpy8uhh2d","sign1xrwcra080uw96q2d6k0ts2h00ea67r6rq4us4jzcn5ft07pnaqpg6emraxsrz5avhx2f0yx6nqdxjdcuq2k4rpm6pc2wklj50my8kqs9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqks5rk7t","sign16gwc46e7z5tqk894srh6ftqjcmcdrlyakfsndzjcglfzfj0eucqhg49mr940cfmcl3xv97jde37quguyqf3te4w78n56pczhvp7ygqpl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjt68pwf","sign1yylfp4238cl9jdw74qxtacw9kxkpf0av9m46ypcsdpj3x4aacsq40h5hj2u29ue7eqhrz4y5spzc6nuvfdu4ejkvy3gx72wa8fyccqf5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzylsver","sign1k9zsymcqutgvf5mr2eawgcn2jrjd8xd7phfvs7vjmeq5s87f2gq9xwjwzwskp45dm76m9n4s28s65cul52pdmsm8a2gdw3mxl79kcqx5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467sszw6mea","sign146t3j8dvsvqhtp7su4vz5eq37lcnx4mffst5juza5cr63mue0cqga8ynd8rcwcxvj65ksfkwykdw5ha4uwpw7xne0lhgu26kjfn45qvy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5qqu7uy","sign10vhh38p52yjz507f0c978wcw2udpnecl7pz8lp9ksz95tw82w5pact68q6ysxqmykae23vsml3nndk26kfekrvsfejynmvxqvlrs5q84fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5hcs2az","sign1ymu39g6vzl7hpxsxfx8zhgek0lhxvhvjaqrrv6agq9sn5njymvq0degtdapr5g8j4580a8c5nn2atxcysv4z4dr6qwgu3dqnmpwrgp9nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2uqk4cg","sign188v5pvfc8878y6dwdkan7urftwz0a9azu4sgsw2hsva4yx8qeqq3ty46appusg6w8ktampkzlc02tq4nlrcwc7v549u9ky3dzrwjyq699xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcxzchr3","sign16yc4z66kwhhyth8x0e9vg3acaxge4uyl67rkp7yynvhmxrpnmuqzkp5mt4nsm36pskrduuh9nm9dkedvldd6ec6g6u2pmsagys8f2py2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6e40rgj","sign1sc6kpq9e0c6h4cvyahlfdyl78fen0505zjhh66nq57w508taygqzcgp25x9cz4gj402cscs0r6sakexed4z8w37d4v9zhfparsth6qes2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs7k5q2sz","sign14clx33087ewh6ak3f7z6jgnlzdwarwm7wnh876zwl3s6mhyxwypn80tx4g4vtlz28qrwnpll3pefqfv6te5h33tjwfngzruszthzzqkhv57zdsct6n3aaz7wqe4aax79h62qd05zv60zwkctds3axy54qhpp70v5d09vtq7xk3uqtv9svq0clsc25dea08jtzkl868rkrresjsw8vf6"]},{"batch_header":{"batch_id":"2182237965335296689127130065814039379220401796729814049448899878827142764409field","author":"aleo1q3vx8pet0h7739hx5xlekfxh9kus6qdlxhx9qdkxhh9rnva8q5gsskve3t","round":2414059,"timestamp":1729056560,"committee_id":"5607799450220365747440341219014045704854447694770887850255269957972557325543field","transmission_ids":["at1e8qqtxur3fqualq5mt57zteg9h373tks2l0wj989khawkf54zg9qcke9vn.45637030403536908962109923828623389430"],"previous_certificate_ids":["2284631317289414437283648911462221461052731136023682797357212744601785827204field","7640149579724504799178648628142014282292963515543852345888340219233552473839field","608273973952282406013871683776384472590760645231149794142228311881221143744field","1616956867154424767034621481372992803149361173663547198705997152850453182900field","3744988224961072246619260975217039311366463770114882060374297741014468138737field","6577718862910080150119947960491238153462480211714925966379827606930918321355field","7760404408778661184630544612168685397816400938918123502180506201791702398307field","5655265469773645344706463922270349025114067095287648525464831887968482188732field","3617885063009428594342996352940933716387125768286697098775479836204381242268field","6992698151059455795368538672954515356271282981084828778017048705614766607184field","3138565386551440886402212532946913578141017454891471372452086094929027394110field","6266548885740113165122662924059163323861750617859160538684049395496797934588field","4367728412536749617228744745675004829164460810922422673367767573375119492712field"],"signature":"sign1200fwrgzwcpc8wpjgawhy3fj7krgznlenz36vlpe4gg5n722duphg6z4qyuwp68sakx5khp2lgelk0rssu2aae2vzezqt9g7wv9v2qnvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2wm6rjp"},"signatures":["sign1fc65hpe9vl4q7n7zs0qfzp9xu6kcykx7yjlf4j758r38nxt00gqaadqn0ks9wxdfcc7klz2wt5l90c6afye4w27s8eh2pgyhtqf6zq5y52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5mkasry","sign1vldl3eu7dj0yu9703hksqc4lcumry0ng4warkklzzvhcrvtekyqyzh82uc9shrnwlhav7qupy74zu5xnfdel55yhhsvg9hxvpyequqz99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqc0f8dhj","sign19z82m53sxqclj7gs9ntfnntk02kdfue9yyqht5g2yqfm0zxswvq0pq3jxhejfn3yn7404cnye5ze2rr5vsvnxxsgnuy3ntl9cccpwp9nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2fxslf0","sign1uepu625hkcjmglkeu36lwhupyc04vd2fynzds0xt428vje9f7uqjyqw8hnv5fsnx7tkvyftjegnqgf22f2awdl6lzjlg8neqygp3vq04fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5cczp38","sign1s9wzycr66nz5q02jh9zn2av5s4l7p484qul3nvxdd2jqawvgwsq9tcly2h8s07fcgzneuy5wnqgqxjqahmx9uyh3aghshh486k7tvq52x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs68dt09d","sign1l77cps8y6ccxuqt6a9u0vjukf7mcxm07safmfza8q5p72666yuqch5h5l6z8kmfa5mtsl6yjvsftcwnvdyters84h7xgk4432nrmsq3s2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs7hyj4kq","sign1duvu5ssjmfu5q5nd0pc7c0chm782p496w55kffj63rgc4s60eqq4c7z6530p3gmt87dd4mjuu5r4emcz44wxdrae96w2u7xya5tzyq35elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzg9a4mp","sign1a0qen9jld6t0cshz88qmrug55ufldcjcy5gn86283ggu6ujs95pf7zemsynat8e787ta80wcn07j4zn9vlt4g9aak0ha3h7rqmk6gqc9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqk7agetk","sign14j6txaavfvmjtufl6kkr6d44thmqu9xmv8afmn6q02d0aczkrvqnld6rfuaqtu0mv6f33y7k2azhvxlc2ae3gucc553w62ddz4zs2q5kdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpys98r3x","sign1gah7dvcrm2gaf45xc5exludyr2q2e7y52c7lvd56fkrpdnqqhspevxzq8ulhnczxhtcd5qw3vkjcnlwq6f73ch4vntmkv9zggw8u2qr7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6fd7ezl","sign1zcnkdreghddg0wc8nxp0y2qzl47d9ar0zqmsy7ms04jggysma5qzmg8g80m3chruh5fc7zcdurslfdwzm74vz2ft2zes9kf9wyec5qa0mqecjdn66z9esugm3wfedeghsty3ymlk97u8rs8hgdzqzuh5puqnjedls3q957n00e5g0ahhmr48hvyzz6qa62daq2sga00nes5que49wy4","sign145nv4zc46pp3j6ghj9zm9q3c6etnmrect38qa32v5ukrlrk23uq5kydypf2s8r7c5nwrmqad5p32fz2w7ylk8ykkdgqgtvdkdn8fjqpl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjshqydd"]}],"2414060":[{"batch_header":{"batch_id":"6933965430109437740988694168569137053083939200936117197609852926348021758181field","author":"aleo1n6c5ugxk6tp09vkrjegcpcprssdfcf7283agcdtt8gu9qex2c5xs9c28ay","round":2414060,"timestamp":1729056560,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":["at1e8qqtxur3fqualq5mt57zteg9h373tks2l0wj989khawkf54zg9qcke9vn.45637030403536908962109923828623389430"],"previous_certificate_ids":["5000320652835566145799122564258286470173916695817213542071340891552366149515field","5678370832756916953998007774739162485287148398592795693241539175196920975232field","27421375625700716481345007537036560481537359345157645424033187389211086699field","2688377525114967928965935955578566480136637707307459722905044086963894433670field","6015248219911982387774795072200086771489762226331806837348947233279407036866field","6061401131833965447010033417921832614067798088561261612399229746830257307680field","198590889747473512308690861647768068444064485417666362902140477744837575130field","2908474744509694752257468661946233604353606952471857330805132998655858022604field","1743126726795796316236148539495940586475675011593448936836762311808601859447field","2324799862895271806582650849328258466881316633483666479169624408600616533499field","4827272199801305427931718525999096846930141669818653488385940428330388558906field","7983661187835694349128436813979024594721029860058172601817395446239722762779field","8407858778344856641688807177089238284677523636926018194017124151660015563838field"],"signature":"sign13sdf46p3aakg7zprhd5lrdtyjrfqdhccdkt20q6ss7j85nynuvqywjhn0rynyuus847qcca0u9m6dflvxscc5udsamrqc7aqfj4uwqel4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjeuwlfc"},"signatures":["sign1ezqapvqm5l4zl0kxgtruc58ds6hlwarluscx4x3vk9zuh39accpmxwkscw9ff6vcsyax2nl38uwn7nq0jdq7fzdz5dghjs4r5h62uqw5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ssx3ev67","sign1t4tznw6j5echvjgngpsy0d763c8vrr9z8rthzhyfkueynffspyp64z2j2gn6s2v40500mq53l2dr9n2v0uam7ddazsyzehc27qgwqqt7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6ujd2mx","sign1tknujcl8tpad5wmvcqxvn8u0eju0y2hmupz3h7qukasr320645qhd3h2v7rmzlguk40u2cwwc69phc8nmps7909k3aqnl5kgxvja5qvkdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpy3enhct","sign1r3q34p066pl49gu0c24k40ne33czfwy5xc0sfuet0e96mdwhmcqlzgpxt6vfdl2jmxyjuykcm6l4k39g0fm679sud8seh7wpsay8uq7hv57zdsct6n3aaz7wqe4aax79h62qd05zv60zwkctds3axy54qhpp70v5d09vtq7xk3uqtv9svq0clsc25dea08jtzkl868rkrresjuwjha2","sign19kzxzpehk2rdzhszzr2j6cy7dkd4vfm04ql3jnq30y8ach0cjypyu4xsx63puvzg77c79d2ed6efyh77428y7jggx8plryreku6y7q35elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzkxcm7p","sign1esf25hhnges93hwh0k5rc7gpgdggj0jjp6xk5ej8y82fdmtfjvqww7vmwfgnhnn88tk62fmt5gwrwgxa5kn0qvhkdzgsh2su0f45vq3tcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqk59kp6j","sign1ezvh2hyj6ph0yh066a7jrslz4ld2r7s074y49ptktv7ph58dxypls4j2ejrlc9etjcvvascac3fppen397rmlapa7nr9wuek38g3wqc9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkhvynpv","sign1vyy67zhxkyh5lvm02jx7djp35jwx3xn87txymehdc8nawfpc6vqq4gm5u3rfdd30zwgsaa2wanv3ccqt2zf6p4n9pfjgjz986app2qtvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2rgzd3v","sign1xcv9mfag32ylmmyuvm3066r8nezd5n5ekyaepv0pypq44u4pfgq6yjxnu8kzzgedqu02779c8cw27jejn3utr4ru34tc50rk7e3kupyy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq547hgds","sign1qx7l65y9h9yncue7x6zge44y3md6dsz9ru7vhh2f3l952hs3yspq6pl8j92qefw5ssnhkd56m0xxrwn5uscyqakpct3cq2d622flwq84fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s52lm9ua"]},{"batch_header":{"batch_id":"1232619116214198098204816232875560987377739846217396757086495058130192230615field","author":"aleo12tf856xd9we5ay090zkep0s3q5e8srzwqr37ds0ppvv5kkzad5fqvwndmx","round":2414060,"timestamp":1729056560,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["5000320652835566145799122564258286470173916695817213542071340891552366149515field","6061401131833965447010033417921832614067798088561261612399229746830257307680field","198590889747473512308690861647768068444064485417666362902140477744837575130field","2908474744509694752257468661946233604353606952471857330805132998655858022604field","27421375625700716481345007537036560481537359345157645424033187389211086699field","5678370832756916953998007774739162485287148398592795693241539175196920975232field","2688377525114967928965935955578566480136637707307459722905044086963894433670field","6015248219911982387774795072200086771489762226331806837348947233279407036866field","1743126726795796316236148539495940586475675011593448936836762311808601859447field","2324799862895271806582650849328258466881316633483666479169624408600616533499field","4827272199801305427931718525999096846930141669818653488385940428330388558906field","7983661187835694349128436813979024594721029860058172601817395446239722762779field","8407858778344856641688807177089238284677523636926018194017124151660015563838field"],"signature":"sign104awgrsdj8c6k3qq40alr6vsh6ecm34kwwqvlyayptavqzsz95qf035kl8mr80mgpd76x2gfddhgeap3ffny9usc9rwcxk35fyhygqx5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ss2aty6f"},"signatures":["sign1pq2chyzp5elcxes59sj2qlqyvltuw3enjen88c5hrx582xep5uqzjjfsk62aztk080w5z302duwn2zjvrd470l5nenjhsnvn4f4uzqel4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqje9wl4n","sign10etfzlvuzqyy7tfq9vphnjngne9t5gducc0ds0dz9wazskxldqq3jxq8vfmke9uvrtvfcwxne5kwnutsldlftueuffc6d9cfzcscsqykdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpyxtwk5s","sign14zz7ax9jg3j7capnlxzta02grk8h8luskdcsm3763weecranwqpec5tpfy9cd8c70a422xsvldzg9qe9r0j3mf66k9tpz86ft45kuqn7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6z60wpu","sign197kdpx7py606j6l8t5mfew4avm9tn30h2n5rm6kvkp7euufaaypq4h2gdjwpwqd33vy5ls4aq0sz40w76r28feqqvyc5veenm6qkqqwhv57zdsct6n3aaz7wqe4aax79h62qd05zv60zwkctds3axy54qhpp70v5d09vtq7xk3uqtv9svq0clsc25dea08jtzkl868rkrresj7eeegf","sign18gtzyv7kttzrhtk847rfkywg88jm8ta58meyzmm5z07ntqe8ucpsndm2njfvrgch4purkj2nhy24r5mrf5rxw53mza0azqj4gdemcqs9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqke9dpjw","sign1u4h9f6rfdsxksxylvgd4gdy75h35egj4fn5us0yhtd30j06jl5pw3h09u82jn0aktcka7q4tn9pfn70mlzrmwv80x4qly7lv4w05qqmvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2zcetjx","sign1ewgckwgw2gw6jp7ev2e44qfjnjx6llrzkk2x4w6m3jqlm07a55qmw8xxuhfwtj7wmaxhytw9cj32t007mfvrhpsd0v8edg4qv26r5qvy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5wpqmhy","sign1zf9djuh30hrzdky8nfplxs2qt7srtmrga8668tavy9dsyxf0nqqzmzgu52gjq3ayvtnjvw9ed3332mjlzud5utnhfjlg3vpxq4cq5q84fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5vnaq6f","sign1xwzamvur6krwpenj35jylgexfzyfx2h9wsf8xzcrt8yzxcncwcp78rcvese4af5lfk73ujmv0khleh5kq20vjggcuexma6dlc7knqpp5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzjpdlj5","sign1599fsdt599uqc3cwxmuerkslapyudg3w3s9jl3c4k4nmqvke65pt8v2w0aj7j9wwy9j89avwnq534c28j8sudzx4546hy3pe3zgt7q9nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2e6n03n"]},{"batch_header":{"batch_id":"3625194531782949063158791953110562455080234359211740435484165965284933251289field","author":"aleo1anfvarnm27e2s5j6mzx3kzakx5eryc69re96x6grzkm9nkapkgpq4vyy5t","round":2414060,"timestamp":1729056560,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":["at1e8qqtxur3fqualq5mt57zteg9h373tks2l0wj989khawkf54zg9qcke9vn.45637030403536908962109923828623389430"],"previous_certificate_ids":["5000320652835566145799122564258286470173916695817213542071340891552366149515field","27421375625700716481345007537036560481537359345157645424033187389211086699field","198590889747473512308690861647768068444064485417666362902140477744837575130field","6061401131833965447010033417921832614067798088561261612399229746830257307680field","2908474744509694752257468661946233604353606952471857330805132998655858022604field","6015248219911982387774795072200086771489762226331806837348947233279407036866field","2688377525114967928965935955578566480136637707307459722905044086963894433670field","5678370832756916953998007774739162485287148398592795693241539175196920975232field","1743126726795796316236148539495940586475675011593448936836762311808601859447field","2324799862895271806582650849328258466881316633483666479169624408600616533499field","4827272199801305427931718525999096846930141669818653488385940428330388558906field","7983661187835694349128436813979024594721029860058172601817395446239722762779field","8407858778344856641688807177089238284677523636926018194017124151660015563838field"],"signature":"sign15tswf73vl9qq6p9qqajdnm6xxpgmln94v7m9k840lwry6uu205p8z6sateds3k32wseuwv7dynvjl39e68dcpxpx2vgna5lquw2qgqykdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpyjlf0hm"},"signatures":["sign1z5hunld6wcfkg9k7zlkg3hd46uqu477rryp46n58wezvzfxflvpj074x4eq9pmghnxt3salrnm0h7t5plt0dxajgrk4a3533d9a2xqn7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6ejxqup","sign1dt73s4yq3tyme7hvp74t057hzy4zcvnv2r8k4t04rzp2uul7dspm73hwz7krprgnffqpedmj3z49fmnk78xcj8e9wwer0wuhczv2sqe5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzmxylj2","sign13dz6c49lfqtl0405sgamv9cpk8q5uvm9mapvkq2ztljcyc9lpcpf35j6e7v3znv84m4putt6cpvnpqptht9kqy4hk2tsgftu43gu5qk5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ss822vq5","sign1amvggd5jaghycywpdvvvzu3edcq9atd6294yp9uxn0ska4upncpl354x350gw2xlw80fnnt3mpqrym5phvmnyak43h6wmk3f8sp6jppl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqj8gl4az","sign1n9z9rjm9092ay9ma2qr9g5frd9q9krcxrnhm95nfj9vn3nk90upxez0qlnyl4vl2pjntu50xj023gwx49cuhjdme9x4a8qllygquvqc9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkthgd4f","sign1vxkyxhfvsqen78mt5cgqsyely6vnstu6mus4awtewvljvt6tdqqfc63dvamqxn4xusdgv3ksw95nkjmepk2r6rlxsz2jegw62gh92qtvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2ua34rg","sign1fkp9gs888952fd7zpuaedy0gzsl8q73r59e96f2u9u4q7jkhpcqy0dlcngawn32fwrw93cgh9ew56j3nvf6rdwj6yvep6pw5ayvgyq5y52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5lshu4d","sign1qjqpuepw3z2c7dsk2c3fch9ww4a3s44ctmnmvqn73j58mju5vcqffxkuav73hd8w36qf0jn8rneh8rgp7wg6ut00fkx2uyn66ax6cqy2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6d60g96","sign13rqyzyct7pgklwwstxyykzv5627pgq3ncukdg27kh6ets00w6yptufpc7tdw238jakmyrnqrdkay9zcpvyq05dkw366wzp9539jsyq04fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s55k8022","sign1w6xpural0e6nlkzlr30sz7f9lrxfshgpyl6gcpda4qmrrw56guq8rfvru4uzp3ypcl7252tdfddh6jxqarvkxzwpxqtj76ws07hkwq699xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcavx5nm","sign1hgwmsw5vay4txhj28v28ymhqkm797qfh9psnm3nlr97ue0fxpvqcqpwgsr0ndrqjd2pykjpsmfq9v876x40xelz4xvw2nflk4yrpwq9nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2ftl0dr","sign1sc0mpf58hfzxdzhs274n4zj4kyxzpt5hgjk3mtx4vt59007glqq5wgd7qt7lgkvvyjstrvc9te0vwd8pafah92yx8pxyys2qa7chxqptcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqkukthjs"]},{"batch_header":{"batch_id":"1047693341435237257835045176414743568227351675449538425709853805002301314373field","author":"aleo1q3gtqtd03fs7chhjdr8c4hf8vkwt96pf3vw28uytsdrnwt4hrs9sg7c62j","round":2414060,"timestamp":1729056560,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["6061401131833965447010033417921832614067798088561261612399229746830257307680field","5000320652835566145799122564258286470173916695817213542071340891552366149515field","27421375625700716481345007537036560481537359345157645424033187389211086699field","198590889747473512308690861647768068444064485417666362902140477744837575130field","6015248219911982387774795072200086771489762226331806837348947233279407036866field","2908474744509694752257468661946233604353606952471857330805132998655858022604field","5678370832756916953998007774739162485287148398592795693241539175196920975232field","2688377525114967928965935955578566480136637707307459722905044086963894433670field","1743126726795796316236148539495940586475675011593448936836762311808601859447field","2324799862895271806582650849328258466881316633483666479169624408600616533499field","4827272199801305427931718525999096846930141669818653488385940428330388558906field","7983661187835694349128436813979024594721029860058172601817395446239722762779field","2182237965335296689127130065814039379220401796729814049448899878827142764409field"],"signature":"sign1jt2255npjm68md3hk3m7vrzw6xf88xupv8p3sxnss64sv3m3suqayjx60k4syh7myemvauv9r93vy6r4gplf53cnkz5mqcrzuau37qs9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqk7ur3px"},"signatures":["sign1lva5ka4vekzx296grdwk5e6ezcaf7pm6rtpassascd9k6cwr6up74drkk4mn60gmde5emcu226v9pc9nn3pur29dauq0e0psjppvcqe5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzpa9h7p","sign1mx0lcr49pv0ahl9s7w9xee4g8jm0g35646jj02j7xta47ww5ryq7c2lm65mfh2zqms6dg5wl88fgyq2tz2gdgn2vru28qwtpmzpmwqukdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpyhwhq67","sign1z59d6892u8dzc8cgsw4sj6tcr8zrxp936ferzz7hvamc4l6g6uqleyxx2xl8wjphg0fw907mfuj27s7dye7s5e5d29hm9q3sh0x6xqw5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ss8apy22","sign1zq456c92gmdmsscn94dpj3zdl3qjzcf7ku8dm22rvjste8n2lyp7pkcnlynlfhgrwv7l29zqzjwp3tm5uft4j6jjay5nwcupn0flwqr7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6elfger","sign1226t5s5j5uxjrmp2tczzt5rm9lymy354cjjz0gvgqaf2v5kw65qdyzshhzy66n0w97ed5ftds6hqvp2qaextkwq4e82lga7ccng76qmvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s26j6da5","sign1yp4w7qtyedhpm936uajtesrncp54u460c2sxf7es82m2hpuftyq7a8szy9lgfhqdquvvhazsdfgf3ln297mkcqc6mq2x9ededn4c7pps2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs7v53ldd","sign1eqtfehn66u8kz48p4grz2aptasutm7448q4kr0lk2wqwrd3w6cp4msrfnjvc3989uxl3260yclqkgp0mz6xsr93wj8q3tqprtrg3jppl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjyfjaft","sign1f4aqrw4mun3ac4ajf8zwqhj393kf4y6unkf5nwhwljtmehhehuqyqdan5rm4nzp8p5hq64ph40wy84vfx7u5hednsdxrxw5l2v07xquy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5frrkvw","sign1w44qq30ev88xvmtwrhkerfstzfp29mng5ylg093wvkg3gj5mqypptmwq52twkn2d0jdffdt3kwlnps2jx4wlrwpz0gm9k96dwlw6qpy2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6fmrwhc","sign1ypye6zynh47cga2k3amfzmndku7te4mjpffzzsgvayzjud4a3uqtsrn03maej2muu47jv5wqmjtf8896wrujvl2znja6n7lytwwxkp84fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5durzy6","sign1vcsytc0x4aktqla3ddmgypgcrhftk4zkp0fp0wruu8ggsfsu0qq9ntdk4ql7g82xtqgh2t8e9rph7pul7naktgzwyzvveftsmcds6qdnkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2sjnuve","sign1t9z49pc8f6qlaj7s9tzh9q2048szv9nznqv6kt9vhprk5vcnlvqswk2t508c6hglvdy56sm8fz75x5qy0xrtjxa63qzqgq75fgun6q699xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqc0dx752"]},{"batch_header":{"batch_id":"4376405578818296783364429436924610551956677457230964503352229688996235098853field","author":"aleo1qc46ca98xxjy34v37ge75yydyt36mgatqup7zrjra6gp9huatsqsjv6p52","round":2414060,"timestamp":1729056560,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":["at1e8qqtxur3fqualq5mt57zteg9h373tks2l0wj989khawkf54zg9qcke9vn.45637030403536908962109923828623389430"],"previous_certificate_ids":["5678370832756916953998007774739162485287148398592795693241539175196920975232field","198590889747473512308690861647768068444064485417666362902140477744837575130field","2688377525114967928965935955578566480136637707307459722905044086963894433670field","6061401131833965447010033417921832614067798088561261612399229746830257307680field","27421375625700716481345007537036560481537359345157645424033187389211086699field","5000320652835566145799122564258286470173916695817213542071340891552366149515field","2908474744509694752257468661946233604353606952471857330805132998655858022604field","6015248219911982387774795072200086771489762226331806837348947233279407036866field","2324799862895271806582650849328258466881316633483666479169624408600616533499field","1743126726795796316236148539495940586475675011593448936836762311808601859447field","8407858778344856641688807177089238284677523636926018194017124151660015563838field"],"signature":"sign1tq5tau3s5cfwaw46n290y0hk5ewkz4mnw7029t30c3mrysa35qpksgs8e8j58h6dzdp2ddcd4js52gnwy00yp99sl49q6vhzer9e5qd0mqecjdn66z9esugm3wfedeghsty3ymlk97u8rs8hgdzqzuh5puqnjedls3q957n00e5g0ahhmr48hvyzz6qa62daq2sga00nes5quzgze6j"},"signatures":["sign10cgvw7390tspqzrjfrqj9z5czpamejnz2r6fj554nake7s4w0qqcmzk9xj05k4n6s5jdnztlvq5cm5g8v8e38f3evq0w395dn53p5qftcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqkzkcpv0","sign1nnql4zs252tkr4yz0msn506f2a95k92k98epu8h49ev2xuzyxspnet5nnh7fcyqrun4jds4usax8tn5k9dwpqum50t5xeqd0yavvyq9nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2ga2pvx","sign1z7wgq6vm0xs8hsps9465snj8j73r5e866438vdn6vep8kaylxqpas88x5gw2sznhql8ra4snhpqacwws5ulp8t9j0v2ejgvs2dzykqz99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqc3u2q42","sign1fkkl935z99gep8k2mvrvr0t5s7j49dmxu2cgffrgp2088pfxzqq023l5r8puqxte6zzzdcxpeh8cc23vamd0exq99cw05jv6n50qwpxhv57zdsct6n3aaz7wqe4aax79h62qd05zv60zwkctds3axy54qhpp70v5d09vtq7xk3uqtv9svq0clsc25dea08jtzkl868rkrresjj3ws24","sign1fg2xkdgnk9ll6mqfxrfcnvempuj3rrzvej6zhr00u5tmtg802upyuf57seqrrk73e2qhxpgyw0qugdu9kalg0ry0aqma5q52u9r22q3l4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjsxtq68","sign1r5wttadmlm3rg5k0wwaxcla9kkqshudr983gc3jkuwmxcmnjauqg8zahy50cvg8dgh8vmv5m0uw8kvf76q9eakjuju7mjc6vj7casqrvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s274gkeg","sign1xv6f3cqaprhfersy2j8p36cn99h9s629zcyxrpzfu8v33je5zgqrp96s2fksttnt5t4f43tqg7vk3rfcv2jsv4jsr4ejk0dsc9t5cq5y52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq565rve2","sign1c4vh7zuj9jtm6nh3ynhfsruf87pd4qztpypkqsaqxwedp2qjhuqjw3973s2pggj0atd8gzgunykfv9el9swerkc2tvkzs7f078fkcpx5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467sszg6mal","sign1d3cfyx99pgmxms40aj5jx5kcqur4z8x65hj6ylaq8h8lzqst9upj0naqd9wujns39hcl9uq6k53sehq3lruc77cyqpnc85c7gd4rzqh4fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s538jcxn"]},{"batch_header":{"batch_id":"6597932540108370401690242854116153078558657794717813742710488613461997241048field","author":"aleo1d37xxnms3sq5qxcnnh3dtvzr35xemjzas4jcytjr8uvymfetnu9salav5n","round":2414060,"timestamp":1729056560,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":["at1e8qqtxur3fqualq5mt57zteg9h373tks2l0wj989khawkf54zg9qcke9vn.45637030403536908962109923828623389430"],"previous_certificate_ids":["6061401131833965447010033417921832614067798088561261612399229746830257307680field","27421375625700716481345007537036560481537359345157645424033187389211086699field","198590889747473512308690861647768068444064485417666362902140477744837575130field","2908474744509694752257468661946233604353606952471857330805132998655858022604field","5000320652835566145799122564258286470173916695817213542071340891552366149515field","6015248219911982387774795072200086771489762226331806837348947233279407036866field","5678370832756916953998007774739162485287148398592795693241539175196920975232field","2324799862895271806582650849328258466881316633483666479169624408600616533499field","2688377525114967928965935955578566480136637707307459722905044086963894433670field","1743126726795796316236148539495940586475675011593448936836762311808601859447field","4827272199801305427931718525999096846930141669818653488385940428330388558906field","2182237965335296689127130065814039379220401796729814049448899878827142764409field"],"signature":"sign106m4sldfwsk5d7gcdnhy7zk6vlxdl9skqufyaa06d73u9dhtqcqekwgg7r9gk6pu634aw04vpy7phjk9yd0vg6gdef4uc3e923vjyqyy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq55mqljc"},"signatures":["sign1e59cgh2dg0llf3hfnhk2s5lmx625fa5qu7653w6zh8ux4wxur5qav0a8z6r5qkapufge5d7pgzq47rxn8j8karpgnn36dqpv9akyxqmvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s294jgn5","sign1fr2n44nelkjeq6pv0rgtmt0aq52y6s3ar20vp4z59qc6fgkvcgpx2q9r6je0kqxu7zc6t666zg4zkxlqyarvakt8977q3lklur495q699xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcyg229d","sign1x7k5nulq0weaaqd6mczudr47z6kpt4rq6myqf93k9zj9hze0tyqtgm8fenspvnxytf6ht9j4vdjnmn5cz852qgxzc3pt54ncnzu8vp9nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2d8x6as","sign1ls03yf6dgfpqf4pgec3jwsw6vcc3vzs2djx2sapzrfz6hzcgusqv093ph443sh2h6uh6s4rfqx6l47tmprtxdfms068hf56j8x43yql4fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5p8yrs5","sign1wc9aemxsxe2ggsr4jvnugw4gczrx3x7x4tg5jxaerxsmyr9y8upvrnl59g0fzhvks4d307nhmj4x2z9ctmfkg82wv7jghd5adqmu7qy2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6z7jma8","sign1nnf6wx3wt95f6tv2qzqqy6umfqewa39srnh5ze2s4cchqd97vsp2qfjp2th3zsd2y9w0ez2tmqmkcwja3xstc95l7fsm95nwz6ltxqfs2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs724e4v9","sign1qnfv34v8qt07p7w43pgg2tg9a242cryaa26aql8r54quj9e575qzrv2qj28ygzunsw5z4pyfqgtvjhf6k4lgth5t9x0fd20dxj9w2qp5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzk8tqwt","sign1aek4llxrua76nych3rpxn65jv59t8z4xx8yjh794fjyvxul6d5qy6hk2fcrl2uw9nwqkza78u6puyaf7n7kl9plltdsag03t7yyn5pptcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqk529jfu","sign1a7g3thn3zsjmn3mftaxlfyfeag874nhk70nq8rqtqxw0v33nfypv8rrfw0zruax2v67mew2yh98fke7dfm3e865uyjxyxdxxu0k6xqs9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkl24yvz","sign1g0j28x0npf26g67lyqdh7gwt64z73e4zh53ksqllc096dvet4qptuwezlql2ngvy97ccaekmy7phu33eup4sq0n4y7m7a5p37qz3yqvkdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpy4q30lp","sign1d43g7p4xwruqy4mjuxe6fup275rg7s22u7fq4we73zdavpeeuyq94krku4cq34zkjnc4tt7ljhm2zjt993h0n62x8hd8eygs23kzkpr7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6pgdgk0","sign1tf7r8n7pmmdcadd4hs8ugfm7ceh52psq0uj9lfsw4ud8z87265qdysm9wz7sased4vqphxfcpudxc6n4u5txv5cfl7esf2kqmqgq6q3l4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqj60355d"]},{"batch_header":{"batch_id":"384072313879710261533192845263462095097000139350805176007704655262055632600field","author":"aleo107mzqjf3w2mw70wz0uy9r3u95y4s7af27jzxdyemz3sf9ase7v9ss6nr8t","round":2414060,"timestamp":1729056560,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":["at1e8qqtxur3fqualq5mt57zteg9h373tks2l0wj989khawkf54zg9qcke9vn.45637030403536908962109923828623389430"],"previous_certificate_ids":["198590889747473512308690861647768068444064485417666362902140477744837575130field","6061401131833965447010033417921832614067798088561261612399229746830257307680field","27421375625700716481345007537036560481537359345157645424033187389211086699field","2908474744509694752257468661946233604353606952471857330805132998655858022604field","5000320652835566145799122564258286470173916695817213542071340891552366149515field","6015248219911982387774795072200086771489762226331806837348947233279407036866field","2688377525114967928965935955578566480136637707307459722905044086963894433670field","5678370832756916953998007774739162485287148398592795693241539175196920975232field","2324799862895271806582650849328258466881316633483666479169624408600616533499field","1743126726795796316236148539495940586475675011593448936836762311808601859447field","4827272199801305427931718525999096846930141669818653488385940428330388558906field","2182237965335296689127130065814039379220401796729814049448899878827142764409field"],"signature":"sign1x8kd8ksxhjkqj37g3cyeq45t2xyv9wd8ygvqt9sdu7yj7nnhmspswgeutyuydd68za8lpk6p7yvuj4knzm67uzt2duj3gf3rflw3vql4fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5g8jucg"},"signatures":["sign13k5zswyuf3phwg6yngjcqvuksl48r942y7qug7whta3m5p60y5prskgyj8avg80jq86jus0n4vm82jpz9ncn0ll2033g237k5yc9vqrvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2u4d5mv","sign16ujv6vyx0yyel6tg58gyu3q60j9ve0yjdkpkhlzxmfussyrfcyquhchrnp6gfmf8hjd9h67226e894uk7j275vrpeutt6khmdk306qdnkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2yc47g6","sign1cxpw6cz9wsfe44mmz74l4g4q76x64nlxpsnjfe2uk4jnv4y00cqt2lnxlelajpnas9d69uqnplkhg5vejzr7f23paxz4309ueuq8yqj99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcpjtqvr","sign1y3kmt4gpmmhls2cppmqvxsvcy6vu07x5ac8m5erfltgd583zt5pazret0pzl0p9k55rype780j7zrd9gv9n5rl5p99sddxzeqmmckqy2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs63yugj9","sign14vcfaksnjyvdzfw577djllrw4qsp8gfx5p3883qc3eytu7r6zuppsu09llvq3fvvga7dj73al0tzausgse7pf47zk9a26nehjqwnzpyy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5jt4sel","sign1dxpuhzl20rc2rdeh2ayztvmzl0splvjjv9u9j0jaes26uv76lsqvd62e8fjtesdzkdp6k3wypel3vwtlkt0yavfqecuvvc6hw7sscpps2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs7rd80zm","sign1nnndhslrc0pnurr6ycynu3lcs9499uwclvetu8drt62d2czjuvqtzacq96jd6pswjfqe6l8jqf7madc5q0vl6fw4p4dqg6axs09qxpptcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqkdhj3su","sign1usakql23fs7ah6g5ewzgypfjankl80ujg9a0lnmnnjreks30j5pf0g4zx04a09uxvut36lng3s8u38uq699dp7ezrtdnn6fx5yxdwqg9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqka7c7tm","sign1g93hevd02gfvqnrqgygqkg83tmaucmhals3eydlxyerf0t9rjuq2n2lm65ry2v3adfu8zdgg34ynpphv6j8059ngmpdfdq5jpdt9uqp5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqz5d203t","sign1py7e3x8nqgmtrxyacdm9ug73lhsp6mzjhhcp703yvrc7a99deup3cv308w2e06c6zdguuxqr3xppw6uhh33u307yh9ylq0prspnxgpykdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpy8ehlz5","sign1g8fhf8pwersjwzf3mkztf4ffr0rw4jc7jmjgm3zr80lwlgl39spy32nmk7u8c3099vfzcxpwgwfyklgpsvxc8krhm9anh5eclyctjqm7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq63vdj32","sign1ulrcvmc2550f0f4drxmqe7masjs7ngqunl62ju77gh5j5qva3cpy92temxpsnxrenydaxljne2ze04wxfzyqp6h5r9tfh2m3nhgexq75g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ssgqj0s9","sign183h3h6s54x8k2l8v83qurxuz6clav2pzgvp66n62js4nu2jj6gp7lx853jwfh9y3t9dtakggqh3wt6pvvfxpx8r4jltm0xymxefw5q3l4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqj2v9wt8"]},{"batch_header":{"batch_id":"7346183160846366786926355374150809034747622376304404229666720839678179964799field","author":"aleo1hdtsgk52nsualvrt4t676m9sydp6zllwe0mr4w5mzknxj3rkzs8slhszhs","round":2414060,"timestamp":1729056560,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["6061401131833965447010033417921832614067798088561261612399229746830257307680field","27421375625700716481345007537036560481537359345157645424033187389211086699field","198590889747473512308690861647768068444064485417666362902140477744837575130field","2908474744509694752257468661946233604353606952471857330805132998655858022604field","5000320652835566145799122564258286470173916695817213542071340891552366149515field","5678370832756916953998007774739162485287148398592795693241539175196920975232field","2688377525114967928965935955578566480136637707307459722905044086963894433670field","6015248219911982387774795072200086771489762226331806837348947233279407036866field","2324799862895271806582650849328258466881316633483666479169624408600616533499field","1743126726795796316236148539495940586475675011593448936836762311808601859447field","4827272199801305427931718525999096846930141669818653488385940428330388558906field","2182237965335296689127130065814039379220401796729814049448899878827142764409field"],"signature":"sign1kvp0yed7e5l0acdskt0xy4a3m4mspwg97g63eweprn6wl4hvtypk9txujr2hj6dx73yrxwk0xg4q5xn3tpa2pmfepphxjyes2mhrkqv2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6yjm527"},"signatures":["sign1ren6g7uq5mxh3jvzrlhy0gul64zt0nkcnqj8e4lxhlunwj9q65p7c494aq5lvfn0hcmtn5n8k207adtwn2la76c7uqek8zph64h8cqankvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq29duhey","sign1u78ntne09safq2egk92pm6x6xl9405ug4cdf0m2tu0ff86zgdqqh89xs2ktua3dv6dlsc3pmjf3ljz6dcqz9npwddtvglw63fmwpgpz99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcxysjnc","sign1pwmfscnfxynu2l7m4gvrwlcnthvy34jkz67w3fmlptudznl3rypslnqgc0a22j742k6tkm2wg4f5qtjqe9xdnym5ssm723qtz4sqzqmvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s28p5dg5","sign14rhzyfsqgak4pxuu44tfrz74yfscxscphs4tng7clu3mr9vh8vq9veddrf5ml4c6gu7zy568ag8ney85t4vcwgy9sqezg7gqsz0rxq84fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5qdy0yq","sign1xzzyv8w4ks6dcszkt4lqgxsjddpjm8f5gpcaygrgalswlzc2juqlsfz6p0c708magp62fq4puvdc4u3l090em0qc5gv5h74dyjaczqvy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5799pg3","sign1al87su64kssw277zwf2lwxmyym3j2665v4ajdsmh89hluayy9upd6mykrjzsprc03dflujldtkgefyyf3sxsj8wve29c3myz5fhnvq3s2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs7ddzs2x","sign13ux0pj4mrfxycq002f7wn445khxmgs7ez7nmnn6tpkde8y0vruqzmw8ya0tnh2dk3rqvanjedsmdf57qls6je2krdrkzfckslec4zqp5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzghhw6k","sign1jwkk7axhg8ldaeu7xqdgnwh3aflmq2kcddy4tysnggxxq9u665q6fk372zpeq7mcpw2h403qjkxe2cc0dmwcxxqmh7amj2qsgdr2wqk5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ssjhftyw","sign13el7gxytnnqnwkljxqktymecv7g5ag7mfdmgqhfs4y2fqz2cqqqcaps3wayn5cckzxkm7u0htylgsnfu6rdvdw98jv38vv0lne54sqptcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqkr0s07g","sign1gr5cq00e0vghkayk4a9hcv453tkevc2h5k7qmsj0qsmenawawqq2gkz4ugdfzfvhc29p8vc4u997cqjxz4mxjl58rlvnugrthaxfupykdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpyfnx4lx","sign14kwl06l5e0ys3rvsrt2x6h0y6gdxx7udtpt9uqlg8900jcrffvqt7nmuyse4x7u40d79379mudjhxucvdkf9e4n3v64ga2dr22ff2ppl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqj0gcfv4"]},{"batch_header":{"batch_id":"3737034014389007601605510900786091865944626973329427365388630942952087232670field","author":"aleo18xwgkgzwvzpw6yz8cdhrvrv6ztpd26zll46kd4kfcd79c9x90grsah3jug","round":2414060,"timestamp":1729056560,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":["at1e8qqtxur3fqualq5mt57zteg9h373tks2l0wj989khawkf54zg9qcke9vn.45637030403536908962109923828623389430"],"previous_certificate_ids":["6061401131833965447010033417921832614067798088561261612399229746830257307680field","27421375625700716481345007537036560481537359345157645424033187389211086699field","198590889747473512308690861647768068444064485417666362902140477744837575130field","2908474744509694752257468661946233604353606952471857330805132998655858022604field","5000320652835566145799122564258286470173916695817213542071340891552366149515field","6015248219911982387774795072200086771489762226331806837348947233279407036866field","2688377525114967928965935955578566480136637707307459722905044086963894433670field","5678370832756916953998007774739162485287148398592795693241539175196920975232field","2324799862895271806582650849328258466881316633483666479169624408600616533499field","1743126726795796316236148539495940586475675011593448936836762311808601859447field","4827272199801305427931718525999096846930141669818653488385940428330388558906field","2182237965335296689127130065814039379220401796729814049448899878827142764409field"],"signature":"sign1frauuns0my4y3glc0fgty7u3yxsjkmnxpnyvwknmlycfr2uuhcp9hs3nud4e6rft5ah6a8p87tqq09v3ay66p0a0ttav7jg4vq7xvqdnkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2yacvn4"},"signatures":["sign1y2g9jqnnsgfdzprpf78xyee0zc3hshmgte550mgk6ncwz50z5gquzwz6gjfk5yfp74zxyhcy8ctmhh2j34spzdapdzz4xnhcqpq86qtvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2ajfh7s","sign1ezfey66fy2k7x6237sgc6fl67f7v320ea9wka38k9lj8cxqxdyqr8gfq570qmyrcjucpdt5y3ewmv30kmsgfdpln9jp6e8hr3twh6pz99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqc558sn2","sign1a6dwz77utzvm6pl6tqcm0ygl4swtz09mfe72r23sa3htdgy2p5pat57w8fraysgsd4qtprrkau7ezf87wnshv3l8jltcfl5u9fefcqyy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5s53sxn","sign1hru0av24ptrp3emdcvd5jdvm5p0c0ednkmee0n3d54wfcu0dnspg2xxfsp974c0hfd9xnvkr3tfd5658vv4e8ztldk95e7zw9gdqsqy2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6h9pv59","sign1l9tv8zure0q46w04dgjct0v37qggyx7nqr538ssxp59m02g65spyd8npzqn4p9500tf9gp9v8yzqnpnut4plefr22m5f5csghhwaqq3s2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs7suwd5j","sign1xcegnfx7p7324cqx66se4j6vj78h88q2z4dh4wth09untyrwpyp028fxzk269zaa89t69t958cs2va5t3qwphzp43r0ff3jh577ykql4fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5p879p0","sign17xwd7htlmjmavdjxwgvnpkf7vx97auaswgwv89p0p3tr6u7k4spg0pkvnsrj3uc0tflejtdd6ruuv4ka6d0j35svyc888p5l82pd6qc9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkfcv5zd","sign12sau5trl6kcrjmue2v6wl5a7877eynu7c7z0mfs5cu8rl7t82gqjg96xjku4qtesqm0ur7nyctsphwkvhm0sgw3kus9dakdwunx4xq3tcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqkn762cd","sign184wfdhr0l6g4swg5799fafr3t450ec46nj9txf9xdt7jfjn48cpcp0rzkwsr0zdr024f0uzk0zu0wy6aa86lzak0rwppgf5k553exqe5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzvfaeay","sign1tvqw3w2zqt8c4sx0xzd6ff9aa99v5w54fmu5se592tl4mgsfpyq84rmj0ka5eh56lywnyxc9qjdejkvzseu0l7ekrt95svp2s2htyqvkdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpy0dt9hx","sign16a5svnqwvrxa5qf79nj66scdzfj5n3gehfqxuvz6wcwcwr952gpqggx845mr58g2mmedxhz37wmtcen0x7k78gsadpkal3fkjhnr7qx5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467sshrgl2w","sign1n6hmqute3vxzvv27w7eue02kjwn664096fudr32csf09yyrx65qadh7t7zafy32tm730lnnt74qxfjwdguwlr2xgmkmjj3wq73ujqqpl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjqf5076"]},{"batch_header":{"batch_id":"753955275865305033939063207991126376490947500486867809415281451294838738910field","author":"aleo19t9f8nla683mzeaz5q2gv5u90zkx9v6azwvya7fswgfkfddaucxqxr0scu","round":2414060,"timestamp":1729056560,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":["at1e8qqtxur3fqualq5mt57zteg9h373tks2l0wj989khawkf54zg9qcke9vn.45637030403536908962109923828623389430"],"previous_certificate_ids":["6061401131833965447010033417921832614067798088561261612399229746830257307680field","2908474744509694752257468661946233604353606952471857330805132998655858022604field","27421375625700716481345007537036560481537359345157645424033187389211086699field","198590889747473512308690861647768068444064485417666362902140477744837575130field","5000320652835566145799122564258286470173916695817213542071340891552366149515field","6015248219911982387774795072200086771489762226331806837348947233279407036866field","5678370832756916953998007774739162485287148398592795693241539175196920975232field","2688377525114967928965935955578566480136637707307459722905044086963894433670field","2324799862895271806582650849328258466881316633483666479169624408600616533499field","1743126726795796316236148539495940586475675011593448936836762311808601859447field","4827272199801305427931718525999096846930141669818653488385940428330388558906field","2182237965335296689127130065814039379220401796729814049448899878827142764409field"],"signature":"sign1xzmqvmpg262kuf0pkekep37tj86y9mqn2d80ajleqnllhfcdsuq9tanlnawzt5n9v6skdnuq4rdn3w7x6ex8e0myq22fujqh73glwq299xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqccek6cy"},"signatures":["sign1dgsq82ahlzk3zxjkdcldte3z4enclqwf6u8lpcqzmam2sw2ktcpjsutdw8rp4l5qtwg64rd5edmgumewv6h2h4axevrcl7y2yutv6qmvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s22u29kf","sign1zrexzxmytzxp2wg0snqypjfhuved72n3sghc5mfdpx680kf48ypw7tkddg6ykx9ja2daah5z98ggxffx3q5tw9drztxs02ltzgcl7qankvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2gdrwuc","sign1la86x5478z35eu546fh3lksmhz633u556yncqfwqzfaeah63ugp0cn3yralq7v3ed2smfvx8turyuhjzknpn094p44xd4a3r86m77q5y52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5afc0tw","sign1n9erkj79lfv9ta0t22as90rjeu59yvpz6atwfc9n7jfwgftatupqteusv8vj3hrzzugtmwgvmqzawr0tgq72wp23m7h5zdej89zkgqu2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6qdxa9a","sign1djkdda49krytek2uzz87ksvkmthj8rrq9uryz858znel9n7wh5qd580pr7cg4fjmpaeytv9tu4quge4ssfm52zax78aaz2uk7zy0vqh4fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5ypyep7","sign14w7v68fs5c6tuzpu5282gy09vcuzxvxhadmt3mjn4s8d2268fvq6z3n8vuapmmhppu9ey8yyp7pd5u9yeyew980c9f87vw4qx5accqps2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs7jsefl2","sign1fwg3ku5h44mlrmpczappnqt4p3hg7l8f2dy2n7g3e6lj8kp0acqm0zxzuv0yxs0krnpk7w406pu7gw33hrdpnaxpxvll8dt8t97jgqt7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6r85c0z","sign1qqre9r4zvr3w9efnlgdl36xxyjfx5529fhqgey7wfyfyw90l5upna9pzgjfef5gmec4nnvlamjdmmxv39f7sp9efunr7r0ww4a3mwqetcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqkq8fqju","sign1mp0x5qmde2h44h3nh6asjde3953s3dzrtwgm3r0lna6lc629scqs60cx76kthfdapem0akv305ym4k0nmx4mekrphzptefaplwlmyqf5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzqc5t2y","sign1qszwth35466k5vv876nkpj42yc20gcph5r4f2l3lay4dt52ersq9a9lmvdjsxh78ngsmh06yynzpy3pz84puqq5yt4axrqzq496eqq5kdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpylvdylu","sign1zsk53w3uwt89wf2x88jm63xqpph88ccylvmcgw0c0lsz4fglnqqpa4xadqqcmal5h6lx2qgamfnj2n0nlghg2g86q4skxy9zgcywxqs9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkqagutx","sign173z0nwvxg946ezkrzem3cph09ycrl5gunaja55yw8zechftanvqrrwy6njkrqnhlrvhg08kf3s293e4mk7vrlshujn40gw5gm0luuqx5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ssnc4an0","sign1q9a4ul49yg7jnkk2q09m6asm37qa0q2wwex2xnlxtndasx72tupghwt8h6wy7drmu5640sf5e09evhj89x6ulzaw3v4ytnhqk5rlqq3l4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqj5nwzg9"]},{"batch_header":{"batch_id":"2833960716802695151265019157537143513285885873922032569490540653222759848157field","author":"aleo1af5kqnf4xt8tm8wdj4hwawq08tr583x0rhyrwcnf8y0jaedw4upswaefgw","round":2414060,"timestamp":1729056560,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["6061401131833965447010033417921832614067798088561261612399229746830257307680field","27421375625700716481345007537036560481537359345157645424033187389211086699field","198590889747473512308690861647768068444064485417666362902140477744837575130field","2908474744509694752257468661946233604353606952471857330805132998655858022604field","5000320652835566145799122564258286470173916695817213542071340891552366149515field","6015248219911982387774795072200086771489762226331806837348947233279407036866field","2688377525114967928965935955578566480136637707307459722905044086963894433670field","2324799862895271806582650849328258466881316633483666479169624408600616533499field","1743126726795796316236148539495940586475675011593448936836762311808601859447field","4827272199801305427931718525999096846930141669818653488385940428330388558906field","2182237965335296689127130065814039379220401796729814049448899878827142764409field","7983661187835694349128436813979024594721029860058172601817395446239722762779field","5678370832756916953998007774739162485287148398592795693241539175196920975232field","8407858778344856641688807177089238284677523636926018194017124151660015563838field"],"signature":"sign1qz0sf2rkuvwx6hleser2nz60h6hrqfu9u7jtcuv4eqxueqxcluqu82fh5weap5j27nzkxmgytvwxma35ntv5885da7uyxwpcq87j7qes2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs76mffmd"},"signatures":["sign10v2x0s8vj4px55886f4dt4t4c4qu5vgfg98sq2fm0zmarjdfsvqk3dq6qctkahr3c2hnew4qxceqz3laagyq3q6hzr52y3f4pjv07qrvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2f9zxt9","sign13j6zdld2qrd7gmh3y5kul3vpxe4dhnksjp852fmhdhdnjlcztypdvnxftfh422q6lqftphclxm8h2ev60nqpnvcjh7u0g0hy98agjpyy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5hq4dgg","sign1eyvtt59ngc2qhqayxmqaev2dyq3dqc5pux8wpp9753ncuxzhk5pztu206egkrw03smnzav8vykm4fvjhhrann9826kpqhew4wkndcql4fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5z38gq8","sign1j7ut7z8y6uqxkm2x6heh4ehsym4eztc9mchtf8kaxdxy9uqklvp7xw87k5ky43u4mujnhwufqpntv93q4qeuvz5hg5gycu4nnzycxq4nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq27s9qm9","sign1anc63707w0zkeq27229ae2yns6gzy3y9zx55v3g5762ht9c6aupgaq3a8l9hfq4elwnwvju9x9wnf888uta6eea0c25qlxwnprxeuq299xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqc0vx5kd","sign1zz25ev549t05pgvugkzdhvzes968jmpgq8r5kenkx7wqtagp6cqxdtl9ja032wm4vvudtjvd3na0ct295dqfvc6996w7amnwu05azqu2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6ngwdlj","sign1xa8vjd05g6utwtmcjl9r02ada6x8u6fq0xj560f2mz2c5s5kyqp28zz78ukamp2mpkkmea6aspufvqck8l6a4d4pt8hctsg9w78y5pp5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzfymgfw","sign1y7wffykhxaqrqmypckrvsdez8yjf8c35tmv4qlt6xj9efv9g0gq7u5ggs99rz08vuqx8vkv4y24fyug4zrggpm0r3nnfewxp966k2pykdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpy083j28","sign10udzyk35ttamu98rcw96jzs8hnnjhrjgsv7j9ldgk2hymhuddvqghfzvvhcln2xxnzv906xg3scpucyk4ycx0ng2y2c68keslwdmvqn7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq62vj8kt","sign1p2vjqzg03y586fd3qckjpxkmsg7gljx2fasqmazesecrraeclspqrwk7wk8krr6rcfkqdwyu8x8vdz4w0y6x6sgrwh0n2rcwqawy5q3l4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjypgr78","sign16r908rhk0jzd7cdum5wqv8m5jrmlqn8kstyurrrezhv9ce2avyppn8tzy3anjpl8srckkj3krzjcmtylvx9e7723sv2u8xgqtdux2qetcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqk3m8khd"]}],"2414061":[{"batch_header":{"batch_id":"1873332333030018133851183902550755179669888593491184483228605635677131740201field","author":"aleo12tf856xd9we5ay090zkep0s3q5e8srzwqr37ds0ppvv5kkzad5fqvwndmx","round":2414061,"timestamp":1729056561,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["6933965430109437740988694168569137053083939200936117197609852926348021758181field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","3625194531782949063158791953110562455080234359211740435484165965284933251289field","1047693341435237257835045176414743568227351675449538425709853805002301314373field","4376405578818296783364429436924610551956677457230964503352229688996235098853field","6597932540108370401690242854116153078558657794717813742710488613461997241048field","384072313879710261533192845263462095097000139350805176007704655262055632600field","25703415787984925685268205605672006314748098985693072579498043986596547774field","7346183160846366786926355374150809034747622376304404229666720839678179964799field"],"signature":"sign10p5yv7hpgd9sulkex52jcatqzjptms0kmwr0te5dn9hrarm4r5qa7s7zy5zmjs7l6v2um3mh3npf9lfc5c38dqfmpj42khg256rwzqw5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ss77fw6d"},"signatures":["sign1xshdupskdfuk6sg0um8l8hzsph64gsr9pfuvtnws7kqacm6n7ypm4fnsrg2qqyqk2qzcsusp2lpt59uagcwu8hv3h0862kjwaxk5kqfl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjd9g7gj","sign1meagyc37l5xnydak032zez9rjynfd7l0vcfajehn5walcjvvhqpl6erdkkjhjjshzutndgu5xjtum330t8xdrzg4gqcvnl8jysxtxqr7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq66kc7k0","sign1xk272cyzhrud6502m87qryh7cx023akcknjezjk4ddgvymazsuqav5wnhctcp0xkrq23nusm97gtqpnkgjgnhtru46psms9k2tsjgq35elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqz696904","sign1hxdl6cs4uv8etpcv5zrgt4gl47cfcrz2tjajh3ssqjaalmrk9sq3xrcdxk0qcqp8a26kuw374qz0zunnkn4st560rcheaatupcqj7qvkdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpy94cxga","sign1kpjyw8a5znvtmaypaht38d96c70kzv59sauwzk496h2hqmd3vqqpke003v26f6w50gk3je8x2ms0rttyy5f7wex0s2g0ygzrvhgwsqq9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkkejhun","sign1f925f54pgm0dq39dvv6d9rtuzy6qhzsuzhzmxq2lchuvh0v4qcpm07jay7tmex5w00t08h6hd3tfaemf4vuresszpfwcngjlz5pwcqvy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5aj6rzq","sign1vcqz9ay3dax2znrk6sqcgdnhhv8002gtk33pcsggx3969yt69vpqm5al04wlaa9awj88l3zt2cve3ux6yaf7gmrdfr40tlwhwmtujq9nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2wr36tx","sign1uy9zqqswlatnstqkq6g6z5s2g56qcu3amft99f5246qywplt2uq2zgx3vmr8mtv67efedpf7ake37h7hx74gjjhzmadyeh7nhy937q699xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqc00myp7","sign1elhez66qas2eysg6vussd4chwmkssm0p3qqjfng0z8y254sllcqe90ey4p5cggzllrpd8n5dawwcp8fmaw5k7skcj6q6jf7xkhhrzq84fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5v9d0ge","sign1ktm2a2u0ghgpj6ypv7pu228xn3sn3kt6yhp5g9cfah0px2hz9yqnffsm9y3nhd92tspd8hrjeahjnd6nmsftt6fyddmqljhz9utwuqxhv57zdsct6n3aaz7wqe4aax79h62qd05zv60zwkctds3axy54qhpp70v5d09vtq7xk3uqtv9svq0clsc25dea08jtzkl868rkrresjphly48","sign1gha6xfdy2el0jpcf9dazraehfhjr49l862y7cqp59sakyjc86qqzhs56ara5j7uqndpj8zgagm2whyaefpndvkayccfs00035fsuyq52x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs60p33cc"]},{"batch_header":{"batch_id":"5021599431286432159336755138493090315928326497197635232419490404070019335357field","author":"aleo1wu56llc4eaw08t63944fx7r69f9syneh30zjaykzna8z2wekl5rqem0nad","round":2414061,"timestamp":1729056561,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["1047693341435237257835045176414743568227351675449538425709853805002301314373field","6933965430109437740988694168569137053083939200936117197609852926348021758181field","3625194531782949063158791953110562455080234359211740435484165965284933251289field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","7346183160846366786926355374150809034747622376304404229666720839678179964799field","3737034014389007601605510900786091865944626973329427365388630942952087232670field","384072313879710261533192845263462095097000139350805176007704655262055632600field","753955275865305033939063207991126376490947500486867809415281451294838738910field","25703415787984925685268205605672006314748098985693072579498043986596547774field","6597932540108370401690242854116153078558657794717813742710488613461997241048field"],"signature":"sign1vk6arsc4sdy2wpmez0wkwk54xvfa0u89a4m9d45ejgvukqkylsq4pc0ask7544rampavmf0fe4qqadc95td7s6wj2h8ma03k224m6qn7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6ft27m5"},"signatures":["sign12yjta0f7ensrye4c95e3q0fqn3rd08a6mhth97y0xcgg9v205uqlk4wz9drlyyagkawyqvkrmp4xeeay87uhvt43szdtuy3hezq8uq5kdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpy4sytu0","sign1hha0pk69wc2x8jtp36q5m0mvq6p6gmupplejzc289jvhms9hegqn5jplv7d9y7sehus5pl8snyypte5wtzl7sl2kgcza9qxjxw7lsqg9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkv0zhys","sign1f23k92mmu7q06qg3fq9k9a49fjqn0jxq7gz7a46yhm3phdn9guqaca9tqpv0tkah2cx25dmjc5dt2cl5y96vxr7hn9rl695ffnd27qw5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467sspyrem4","sign1mlvy0ujuttchu0h3yszadje4wc9lwrqmrzjezc94dd5wv4sahcp2tmxs76k2qhu4khs2cn960a6d5c3fac3jx6jecyprpx0hz3nm5qe5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzupff9f","sign1qkr0z3v8kc46cfc3p50kvq0h8aywdp573rga0kzp56srdjlxduqd80jtn0vxtgpe3lx3tsc8am22ytxt6u8jmwhcegcws9wtqkpevqfl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqj5s4e05","sign1g7g85ggyk8eeera8vfsgjsmr8mhza824ugmqqgxhdkh0kr9tsuqpj3eyk22s4zycg7n79km0gthshnmk9u4zhgezw9d5nw03fcf25pyy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5fsqk4r","sign1ljnksv4ay4xr566dlxt8a29uq8r3pl6yxdz3rq2w6m7s7nw54qpap2n9yzcqyd4xcfdddkn5gyawf307eynkkjfq9xglzmhztq2fuqh4fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5dw0tgt","sign1u6nue7ja6vxlptngujus63n0ygn93hy4zzzhudmyrjyr4slexsq7p5a9hh2l4zt7293s3d7gxlt6t8dxsxqfcr32860ryknptuxvqqnvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2uq2uts","sign1zpqyteyj50axx77n26970ht07fjtuqyq3q7zv7c4xhu74us4vqpymgm9wy98jup4d504zcwxsrvax05y4p6xhndnsp3txtg6u758uqdnkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2s5m8gq","sign17mffefz7etxsak5zw5ywyg9geg6e59vfvfx26gxx2dk5k5ykc5qz0e7apq8qkakqv5nwr8kadyv3l4zygq2p3ew058f2hmpk5w6jjqj99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcmkhv6a","sign1p4hc8arnw5ma9d0uwlpjsl6cxpa4003ehmnlvdkz6smm6gvtqvqs99ar2qtu56l2t8jnl958g74324dwrta5drr4365y47dz8xkhsqy2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6paq8x4","sign1d2thjdk6pvga9ydghguw5jfex5xkzjhtvgsr7dn0jpjfselm6qqhy3gemvpfgwkg8397e9qpje5khxvxfzdsykd20mh4s7tk98207qxhv57zdsct6n3aaz7wqe4aax79h62qd05zv60zwkctds3axy54qhpp70v5d09vtq7xk3uqtv9svq0clsc25dea08jtzkl868rkrresj9kden9"]},{"batch_header":{"batch_id":"658923340114891726635091301035053850382275420208453938502999474664931029264field","author":"aleo1anfvarnm27e2s5j6mzx3kzakx5eryc69re96x6grzkm9nkapkgpq4vyy5t","round":2414061,"timestamp":1729056561,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["1047693341435237257835045176414743568227351675449538425709853805002301314373field","3625194531782949063158791953110562455080234359211740435484165965284933251289field","6933965430109437740988694168569137053083939200936117197609852926348021758181field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","7346183160846366786926355374150809034747622376304404229666720839678179964799field","6597932540108370401690242854116153078558657794717813742710488613461997241048field","384072313879710261533192845263462095097000139350805176007704655262055632600field","3737034014389007601605510900786091865944626973329427365388630942952087232670field","753955275865305033939063207991126376490947500486867809415281451294838738910field","25703415787984925685268205605672006314748098985693072579498043986596547774field"],"signature":"sign1xsgwr5xx77lyuq620chlyufrm2p5r4ncscdx7gscnxd4h2tzhspxdq8wpjer369n2htculxelv9d8gux0rghddd5cqwdccm77la32pykdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpykghekw"},"signatures":["sign1z7zfn0v6r49xgyhxdh9vn8l59jdejxzdv40t6jlpf5f6x8ckrgq8402hmd3ew0kr4yq6cpca8fj4a0xhc2jpkt57v2c2hzncdh5ccqe5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzrp9kp8","sign173pe0nwefjjan4k3ysegcte7erd8d8ysc6a0av64klwy5l970qpah4nr4mxa2zqtlguezecstuw64lxv8tnxc47dh4tewdj08748gqt7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq63vfqqq","sign1cpcg9l2a8uym8w7pg6c4da88ex8mm3v5l9sq8vq5zl4tzlmnk5qnrp7etgqwxczryzkrrp22f58z9madwqfdaurun0xqrxyh8tkxkqs9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqke65c2y","sign1vk5svz4n5ew52f8q22fqt5c86l3hntynq0ryypn23etyxv7gesp4cllmrg97ykytucpfan6wjdtz2dfpgn7qr2gp69uwcfdna45z7qw5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467sscurch9","sign17dmeykxfd6l845u2wg43tppwlzdjne82dqa7vl54upz36xurtqq96h9yw9hx36jcnpdm9ajw5hpgu3sewyddlfhpnxtfrdqxnn9rzqfl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjxj4ceg","sign1gjg6uashj4jgeyyhrddk4xznq8k44cga05s79609vmdtgllkqcq3w8lwqu03r6wzplyc7g0u4a8dj7ad5yfr5jsjge5cllj5dh35gp84fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5rrwdvs","sign1djasl9kc29pk4ywzzzkkewwjzwmm0zc8v9wh9reh5nspu5j3euqxu574cpuvvctc53829amcwt5phmq7wx54mx38g3k47cuhu4eejqu2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs653ld9h","sign1d3xjppv7l6nadfm348l2zryfw50v7huupruzrsnv2k0xjrasw5qyv9thlklcvls4g9dex5fykjkpyhnu5n03n7feusvs49y44mtzxqnvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s29v7evp","sign1gse0vl9te3jn3mcz4tnwss49qy3cqx80hrltca5xv23ueljqlgpd9z7jzxda04sx88u80l7ywfaps9u6lxcs28ntvukwjd49e2g42pz99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcae7dt9","sign1kvx5zmualt5n5jm73hd6qmnpcyqw3zta8qj0xf4tnntt3y0lnyp982ey86fedgt7zvn5jfp003xy3lgx2gdxxzjsf02yy4uxrhh0cqdnkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2q4cvwn","sign1v2hfs0gwrsp0dqrt7gzmsuxwz5er3urlw5ft5mus0yjq6rtwpcqsqydkvxz98ycvmm2639vcpzzln3mrrfunhelmnvy4jqexpx0zxq5y52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5culunr","sign1ssdf67ed3rt2yt48eus5hhjf8vjjt6s27dgt2jn6u4g8v5xdwvq5saw7q63a7j0evwn2mx5xpcjycxzjeycz59jyxh3ppcpu6cesvqfs2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs7g74czq"]},{"batch_header":{"batch_id":"5787673979679948377579261798809042887560136347237249368768681591236899516585field","author":"aleo1dsrv0z6wu9mgzl5l7wh62rwmrd4yt3zva7n4ayhvg02luvtqkqgq5tw209","round":2414061,"timestamp":1729056561,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["1047693341435237257835045176414743568227351675449538425709853805002301314373field","3625194531782949063158791953110562455080234359211740435484165965284933251289field","7346183160846366786926355374150809034747622376304404229666720839678179964799field","6933965430109437740988694168569137053083939200936117197609852926348021758181field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","6597932540108370401690242854116153078558657794717813742710488613461997241048field","3737034014389007601605510900786091865944626973329427365388630942952087232670field","384072313879710261533192845263462095097000139350805176007704655262055632600field","753955275865305033939063207991126376490947500486867809415281451294838738910field","25703415787984925685268205605672006314748098985693072579498043986596547774field"],"signature":"sign1jmkm694att7f87pcun8cm63yldv9mxsqxmcvmdchvlas55uff5q7ew3y04deygd7vkt390qqpeyzefqvd5xy0j5cwk8d9lxkxsd97q35elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzv606zj"},"signatures":["sign18cwrfcdxhj6gvpclhjx68dzl0vclefhrkfhyzepag2hty8em0qp8u5pul7msdurch4zfmxv7h9x0422gzpc8c5s0gn3c5f35dkmpgq5kdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpy8smpem","sign1zw88t8ulayave7ax06huzajnl9hdeejzkxk88t4x2459dlaazcpuakaqr9fpg83qd6r30vlgt585qm8tav233v5se73ltjaxrhy5kqc9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkpnznuf","sign1m5m9ls887j30tjy58hfdmp7pu4u2p5vv43je4rxhmptsj77fquqr2tzh3gdljgfyawhvqpcnwlww6h467m47yl0jlaw6fg56a2545qk5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467sselchpf","sign1zmxrjcl5g4vgy2ejam382t8t922kjceulpyuze9sv4vq6nexz5pxrsc38p8gdywsafms5rnyplr6gzqmj7tgpymwjcmqqkm98k3kkpr7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6p3m6fa","sign1n5x0a8taky3vqn9ep7chuvkdy46wjjdpw2c29mjgnfu472zkzsp3c43whwehaxsz4lmrkx0d9es9vf6vwe6j8ekkr3dkaa95pqaduqpl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjk8j207","sign1arykd8w6jsr7mpd8uj9wkgnswj7fn26j29jkyl828rtvx0j4ecp9v53d0nju94mly5yrvslux0cvwgfptmuuztmctvgkql4ynpsewq84fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5kj557f","sign1d50nusgpv5xacgwyrf84g3pccue64yurr93zfz8ylt0yc66kpgqs377mnpj2gq289f4tf04jzact9tp0u0nw4jtr6amft55kqr3xvquy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5kzck0n","sign1saz8uh6l4jhls0znwj32nd3ls7058r8jxeeugeecaxpkmrpt0qpkw8ys0zuhmwmefd3hx9z3nhzmx7y5l8tpzd2qrlptggfkj6xvjq9nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2adr7ee","sign1dt6cwfxtc97z5xdxaeguskv9ennylrgsp66fzpcfx9gtzk0e2vpye2sxmssdlh4xr7yuvz2j3p9dc5uy5pu5l49kxycsj6hpqznq2qv2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6dkr30m","sign1eu3dugy3t9n5q57uy2xqpypwddfc70x4gn0tk8cvh8v4esh0sqqcyufwsk9vl84h8v09t38pzcs2aayk6mr80hv2x5c7k0k3j8mzsqnvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2kr7x54","sign1n64kpv77fz6h6025m79rnz7er8uwkj967edgxz6vn0xhqp2usgqssv3jfdp5p9gvqqwr6xa0qya02lqtr4a7sue07n8p2pg3se56xq699xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcdr3dgq","sign1te3krgjp2jvvd3jlwyu4ahpyzwlevuxe7g734r408chjm4fjcqq47x6qtmk7v0s3wlhyfwpnrvzmmpqkxvcrx3e0hp8swcm6fygqjq3tcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqk2at4as"]},{"batch_header":{"batch_id":"2355727057248138228337102165271013656898210569825776199748013689446439744475field","author":"aleo1q3gtqtd03fs7chhjdr8c4hf8vkwt96pf3vw28uytsdrnwt4hrs9sg7c62j","round":2414061,"timestamp":1729056561,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["1047693341435237257835045176414743568227351675449538425709853805002301314373field","3625194531782949063158791953110562455080234359211740435484165965284933251289field","7346183160846366786926355374150809034747622376304404229666720839678179964799field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","6597932540108370401690242854116153078558657794717813742710488613461997241048field","6933965430109437740988694168569137053083939200936117197609852926348021758181field","3737034014389007601605510900786091865944626973329427365388630942952087232670field","384072313879710261533192845263462095097000139350805176007704655262055632600field","753955275865305033939063207991126376490947500486867809415281451294838738910field","25703415787984925685268205605672006314748098985693072579498043986596547774field","4376405578818296783364429436924610551956677457230964503352229688996235098853field"],"signature":"sign10pjqhacuevu2czzvzejf65n4jycydtwkud4kjpzjk2v3ys57vgp0q24d4hqg35u497t0puacn5u4n8gk2ztwfgyqg37mlzs8c3zywqg9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqk3tvn6t"},"signatures":["sign1xymlftx70eesnhdg0yytng749ajp8cvujn8zd3dt8ugeugy7mcq259f4f8gregcqrhz0u26tdj2j0698r9eq0mqex4kg4deze26jsqn7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6fh0nkl","sign1gu2t076r8tljvulzc0wzr4h8fq2ls2gujmcfgyad8rta8rtfxqpr3t7586e37k2rhm9aaj2px3kr3e0m3u3h3rxqspjhqhavm6w97qe5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzxaudey","sign1h2jjqah4ulp2xh6w6fglzcy7fhlwmvhfmrl85mvvax0fsq8yevqnyuvrssfgnkjv8slqctytnajnk0ftkf8xrpufqm0nenm30u7dsqx5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ss6kgr7r","sign1prs794p8p89z5wev576rzmac0tfq3q7s03hjd8mc5w0arnkccspp32q0zkm2gyh3zfycq53q35lygum9wfthcqxufwh2vgqj6mepcqyy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5638va8","sign1d79nednfyqj95437ygxvarv8qc0a64c33hj6hhkgshqwcjcgvsp2zx49m3zp72dnjc6d6hgfu62dc2zezslms0dg6atc632qrw7nuq9nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2qe26hy","sign1va56r84eu9ygw2y4dpt4qwnlw522n8ethesvszntv4d0n7zkfyp57xv3m4958u42t7s99tppd6eagkgt3r0zuh2hau582jsj4ccscqz99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqccq9khv","sign1qucenkydst7nvhgcmph65mywvnez6ek2cj9ytk0k2767xttyeyqru9fp3g52dyr3vuy4dak6zmnp53em0lhjgyyleku6lwscxuz62pykdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpyv07lvt","sign1487j2spxw39ldxfd9dsuzfrf8dce3fa6mfg9sve3wzynm6zwfqqle7m5u5apxafwkpvmp0yrkvelglltsm9ja6aadgfndzd3p0zpuqtvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2m4qmat","sign1tlj7ekj4vlvtmvsqy08n98q9dy3tnxgas0wshasqquylmjds8qp67r0e6pjcnfs0kjr66plrwlnkmp637cf0c34z0eennwxga02ckqel4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqj7mxd50","sign1ez5dp4edwdj835r85nvhaaqj09g8wcrsgmp06lazgerzyvdc4qq8qnscclc480fn69dzukwju76xfzldyxn0p8uf8gxdz97l2v4dsq52x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6mqyana","sign1wpavr8maqy5nh5hfnfrate5wc6wrfctytefa6ryrsfe38mqjgsq96gry5rm5x37s7z3303m2m2ctz93029p0t35ru6rqv9pqxu73vqh4fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5pgrrcn","sign1kkdf7n4rv6f5kux32xn3kl90z8g5ya8fsc50p3acr47jkxvwgvp3ju5drgmdep9s7qlx89nlcn8dyk8jwj69gx0r888h7xjfhuqlxqfs2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs77xr4mh"]},{"batch_header":{"batch_id":"2673518712274515398354726341315183266197647227599002744248963819463701645925field","author":"aleo1n6c5ugxk6tp09vkrjegcpcprssdfcf7283agcdtt8gu9qex2c5xs9c28ay","round":2414061,"timestamp":1729056561,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["6933965430109437740988694168569137053083939200936117197609852926348021758181field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","1047693341435237257835045176414743568227351675449538425709853805002301314373field","3625194531782949063158791953110562455080234359211740435484165965284933251289field","7346183160846366786926355374150809034747622376304404229666720839678179964799field","6597932540108370401690242854116153078558657794717813742710488613461997241048field","4376405578818296783364429436924610551956677457230964503352229688996235098853field","384072313879710261533192845263462095097000139350805176007704655262055632600field","3737034014389007601605510900786091865944626973329427365388630942952087232670field","25703415787984925685268205605672006314748098985693072579498043986596547774field"],"signature":"sign1zzylk8xzjeszkx8w2pthw0vwnrl7p8l2w7p8eqg4xc0wcs0gssp0psme064xadm4ppdy854t4na08m0z8ddq8qrh0nmktna9v96dkqel4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjj6afvx"},"signatures":["sign1pk2nthd9fpqusqcsmx8j34m3v53uywhacj9266mpsfg64cfjryqf677snds6p4k4njw465fvuhw4lpqevd29hnt6en8aax6gjp567q75g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ssuv3gql","sign1d4aq53m0n3e08w5m03f7vg9h6kclpx5texutps7jsy4p5fam2vq4rhuzu6egn8ra0w7ksl3skgw9qddhyvftdpn2n07t5jnmes4a2q5kdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpyz0gpg3","sign19j2gxeehzcfsf485fthpd7xgcsswetvhyjwt6hgcxwhp36ycacpe9kwuqp5uydn7tvf7mesfm0p95g0nyv2xhrwqv7d60tz6fk49qqn7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6ugg789","sign172fhc0jqq2r04d3larr2m3qvfwdtrswv8uxz53kyhunm0yhaeqpx8jru4xfjxhsm28dca4r8rmkw2e0wkzvxj95d58rntffvlgfmzqc9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqk2fs7j8","sign10cgcc4wapey3p8n4ln829tnnyn5kctxuer5qdl4w3dc8uh5ds5qy8k0gm3qdsft8v3z94drqvqet8v0kgs7lmxe0h75c0jmvany32qyy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq598p7ht","sign17ey72tu330ndeln46r7nyfyfnreuv9ygl0gj8sk2j7zw0uachqpjp3vzy7dszrry3xgwr39z6vjtfhy5rmwjezdte8xpytnduv5c5q84fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s55zf6a4","sign12sfgx8975p6r6yv2c67v2grzxw8qya7eem2t5puuk0sxzdwpagp65zy9rrpfar6v5nnu22kh669dnrskx9qpxrzvmd39zk95hwv06q52x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6r66l3g","sign10z39dk2atmqnnwxhk6k3dve8rmtkmk2488y0tc5a3uq5swgdzvq0wldptdcgucp6ewk2chcxkks5dlwlk3l8my8dg26aj7z4edhuzqz99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcyz47lu","sign13y5kxluzkfzygf8qwlc4llt06ya4mdekjnctqmg46v8ryep7uupse3ehh7nhufl8exknmz0cz6wann46c79yjxrrnv72cxlpnp9xvpp5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzgdf8en","sign1m8rhekg2ahyvzxlyasl7rzyfx8gj64ulrz0e7g5rvwx8w23suqq593gt7ayc3gu8sq38vkxhchdwg2temu7872azq09sg35x8xl2gq4nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2kjmv7y","sign1clnu2hn4qwxxe8e9cyts73j7m8xeh2ytysjl2239l9gxetw6yvp89t488w84q3nu6zwpw5lnzlj9g4y556vhe3s592uyq60m93q6xqmvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2adwukp","sign1970tfg8867jyx2v9kqg7tkevrhdn9rvqa2dpx9sr5xpjrqk8lup6m2n5ysjphr2jpegzxanx5wu75v4377x4z0laf6e8lkkq4lkscqptcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqklmngjd"]},{"batch_header":{"batch_id":"2409657726378783286112337974757762727034362624005334766411227987602260157485field","author":"aleo1m5vc6da037erge36scdmefk0dcnrjk9tu04zyedfvxunwcwd3vxqtcy7ln","round":2414061,"timestamp":1729056561,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["4376405578818296783364429436924610551956677457230964503352229688996235098853field","7346183160846366786926355374150809034747622376304404229666720839678179964799field","3625194531782949063158791953110562455080234359211740435484165965284933251289field","25703415787984925685268205605672006314748098985693072579498043986596547774field","384072313879710261533192845263462095097000139350805176007704655262055632600field","753955275865305033939063207991126376490947500486867809415281451294838738910field","6597932540108370401690242854116153078558657794717813742710488613461997241048field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","1047693341435237257835045176414743568227351675449538425709853805002301314373field","6933965430109437740988694168569137053083939200936117197609852926348021758181field"],"signature":"sign1x5de72fwklxnnj9w4yga5s0ljka7uzsrtlav6fnaw4u492a9y5qe7kvmj9fru8ava3ytrgm7vra8e93afmnc57krvm0n4svq4dqw5qptcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqk3j3xn3"},"signatures":["sign1wjjz6xsjpqmg53zxaqdek6vt2dalnfk8v6tum69pj6cxmalh8vqr05f54ut6du4t9ngte4h9y5j7g2zxr7dl3a0uh3vrlfnvdf9qcq90mqecjdn66z9esugm3wfedeghsty3ymlk97u8rs8hgdzqzuh5puqnjedls3q957n00e5g0ahhmr48hvyzz6qa62daq2sga00nes5quc0dvrn","sign1hccs67yvydtl682zhyq29ujutc5kh80d0y85pkq77r0lwsufuuq46jzrrk52ar3aaagqm3y3s7xh50gpzhfq6z6rljx38gz6ep9dwq5y52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5y9fxsc","sign1nmth42qtf9xkqxx0f7l8q9fp23e2hyetpdhfarnq7te7c86savq3qr97635cn3f7f07cs00704k8z26rxs5np3u28dx9zgy07sv75q699xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcx34pcn","sign1ldnl5699ghfyls43mu4w9xcc64u2fem2uhe5useejp7e0w24tgpmms0djrthjz4n752urh7sl7hjeqq00ymse8377erx3vtmg55pjqdnkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2uwek5a","sign1a8ea5yng0qxntl3xxuzt7k7u2aph89nst6ac3la2ccepdlc9qqqzp3j5gsz74ake96gzpdcfy2k2v0sfa2hcsh7gwsht26t6zh8csp84fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5rqftup","sign1rdrestwxxtrs44ka4syhhqn0rp04vylxqgml3sdywf6akvp3gvq8sv8yjwjc2a8nhafg6kecd5rzr65gryvxn2gkkyzk48dcejy6xqwhv57zdsct6n3aaz7wqe4aax79h62qd05zv60zwkctds3axy54qhpp70v5d09vtq7xk3uqtv9svq0clsc25dea08jtzkl868rkrresjh8532a","sign1ruj7rtt6zk5recy7m220vrtt2ee06n9xj6nt5zngavu6frfv6ypytcrg4xqfvmaylu6gx96gsylh52ys7z0eewasglvpcgzmu8xqvqc9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkupscfg","sign1xlesmmma6wtq0dgzxdcg9ta0n2q7ewunxdtlphsxc05l9zl9rcppam53r4ulzvu9z3ypwtlv4kannvzhvtdek58f0vpwjypzc4tnyq52x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs62l6knr","sign1n7arw2q6z2c6lujq4khk3vrkmq9ddqn6zrk3w2d5ya7ujvxgjgqdxs25dd2jn5qxzwkpc0ekthhkm39fxzvry8c2nsyq2qpt4fwjqq35elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqz2l3vp4","sign1xmutyvmu5cdqr09zap8xn8svq2q4y3pxdht4h9cw3xqg99makvqg9l4xrvjw5lqq2mpnnarhclnq7vt4xwrhknm8gy9z0ddtte7rwqnvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2qwg8dl"]},{"batch_header":{"batch_id":"1147444120455701470930763712255746731365910729698800497927072817478339987039field","author":"aleo1qc46ca98xxjy34v37ge75yydyt36mgatqup7zrjra6gp9huatsqsjv6p52","round":2414061,"timestamp":1729056561,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["4376405578818296783364429436924610551956677457230964503352229688996235098853field","7346183160846366786926355374150809034747622376304404229666720839678179964799field","25703415787984925685268205605672006314748098985693072579498043986596547774field","1047693341435237257835045176414743568227351675449538425709853805002301314373field","6597932540108370401690242854116153078558657794717813742710488613461997241048field","753955275865305033939063207991126376490947500486867809415281451294838738910field","384072313879710261533192845263462095097000139350805176007704655262055632600field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","6933965430109437740988694168569137053083939200936117197609852926348021758181field"],"signature":"sign183nyjhvd0m4fq9jrxy5kgcjdcc2f4u9mnqrmtlxwecsvvfc0luqm3wyt0nlpjswwksxqcdvejt2m6auhdpfd3gp97xzuvyputxatcqd0mqecjdn66z9esugm3wfedeghsty3ymlk97u8rs8hgdzqzuh5puqnjedls3q957n00e5g0ahhmr48hvyzz6qa62daq2sga00nes5qu3m2rg6"},"signatures":["sign1v638eyerxgtgf4ejusmnz26nqtfq3qs2khy79htejrsr4hpplsq27vuxfrcknzy4z0w9zg8fmxfnrqx3l8642pxaf2mghunzw3kn2qptcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqky9ed9u","sign10ukck9976rf850fxs7z296tpdv9uch87vmf3ezmsrzxx9lwzkqp85mqkt2j5k5ncghmewcf4atgtpgw4an4xajstux60qjqlrv6vkq7hv57zdsct6n3aaz7wqe4aax79h62qd05zv60zwkctds3axy54qhpp70v5d09vtq7xk3uqtv9svq0clsc25dea08jtzkl868rkrresjdhmrq6","sign1008576klzrn58eaz2aexm4lvwdz2ckmcvzy9zfktza0cvxmw2uqp592yuu5kf4vfg4jcfgphzeas5cdpgrdaza7zfg3070wayhfg2qvkdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpygs9tmt","sign167z0wpegutpcqyh8529m0dak706druzmc3ksqu02dmkj84w9sypukmyhsmvnvry2c8q492agmrxwyjjf0qnxlt67rphnzcz8ft3ccpyy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq520089x","sign1ea750stgxqshhdv0wut202m9dskvpr0kqjfjjc73fplllajg85q4yh2qmj05ng7nfz489m2qlprgqnlpyw3vup4runuf5jc90x0ewqnvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2jwj5u9","sign13ugtjuvq6e2jslwg3fsyueaksp5fvgv3es9tjxnnh3gj3nrsxcqvy88exjkg8tkt82tmwcdhu54smhfztv767nyvgx8zuufx8vn9qq9nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2zp2kf7","sign19v72fg6438yzra67cgfaxkc2tjukse5sfx4f7lhth0ynu35yaqqwjwtry5gas8mp7hjl7zcfqxcqugl8v596v7nf34y5fzq4sffvcqf5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqz5lhqhl","sign1luyxmtx37v6h9q3c7q6z03mg0dzr60llm8ha4s0rllxqyr9l0uq25zj2w9vp67ea44vmdufftrc9vufmm5det9afp4x93llq7mcjxpx5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ssptuaum","sign16czf2j0whe705jnfujwakmv6u6fvdacjmtmwxfndekp59zj63uqdh86ksygr6266n9y5k7c2h4q3pwcwa894e2zg2p93p48jqna0qqj99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcc98vdt","sign1k8hs228ppzfyh927y4exaq8l4dp5u0dkmdsv9y67l0dfxm30yqpwprxmec6qpf5thvakkktgjfdcpucvdsgcgry0nr26d945t3wpsqm7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6np9skt","sign1hg20hs67j2dhlu80ah9su4l5yg5hu9xjxk7hamxvkcukl2xahqp4npgkjczrqml5ct6jwqcv28ugmyunj3dprmq6e0987qhfez9yuq3l4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjz9xtln"]},{"batch_header":{"batch_id":"3308272729573063666725220787072185977982842256694541719108872762435055540697field","author":"aleo1q3vx8pet0h7739hx5xlekfxh9kus6qdlxhx9qdkxhh9rnva8q5gsskve3t","round":2414061,"timestamp":1729056561,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["7346183160846366786926355374150809034747622376304404229666720839678179964799field","6597932540108370401690242854116153078558657794717813742710488613461997241048field","1047693341435237257835045176414743568227351675449538425709853805002301314373field","3737034014389007601605510900786091865944626973329427365388630942952087232670field","384072313879710261533192845263462095097000139350805176007704655262055632600field","753955275865305033939063207991126376490947500486867809415281451294838738910field","3625194531782949063158791953110562455080234359211740435484165965284933251289field","6933965430109437740988694168569137053083939200936117197609852926348021758181field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","4376405578818296783364429436924610551956677457230964503352229688996235098853field","2833960716802695151265019157537143513285885873922032569490540653222759848157field","25703415787984925685268205605672006314748098985693072579498043986596547774field"],"signature":"sign1ul3vzlm3f49smlf28fc9m48ms72qrz4pevnmzd7r8y7hkgdeyypww938jjxcayu8nswujg9h6akaseqn5nr0trk75nmjt7rnn3rlvqmvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2g7rcxa"},"signatures":["sign12h9kwsc7gfsny4ljyvl5x6vc02xw7hxc3mh03jnzv2t9p9727uqnlj0kdc7gzm3762f4k26hd69nxjwa6vynve4ww2vyjd06782juqvy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq50877nt","sign1v4gvmpmaswn2sr942p7ezm6nwhxrks5857zp9kazdrr7lu3heypt9dav806q2n787uqgyqnxw5tmzzyd7tlauuxpj38ffl9mj7ew7q4nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2md46e6","sign1jkqs5jz64266w5ncl52appmdgkj405px5avarr077ls332dus5q2t04dev8m8vpmy5240dfkunzv59c9hnkf08j7pnpe20rlt3deuq699xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqc29w730","sign1exksyvsnlypktcqm2438uj0f285tpxzcup9ptya5486sh63a4uptkalx65ymtp879xnzjkm8sfy56jhdu8dsz6w2dq64qcvxu8njyqu2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6vjfypd","sign1x6ss0tsfvezy5w5zvkmndh78uc5aqav3dmpfzndreqp9dq0xygpr9hyll9ztqj7mjwf4csjafpagu093y2s8jvnf6af0yupjsshsxp84fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5rmw225","sign198cdumxk5zr02m4qtphzc29tl9fgsdeus2jr5vddvgmspzq2wvp67gcatxgdc77nmt8tgew80zeukpmsz9ayrs8whgkms0xs8e792q3s2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs7uq0j6e","sign1ye8vq7v68gfs062vdl3mp23u073hz4h098umvcgegy3d07sanqqrvjr7akzt590ngawd945a65kqy7d6flxk8g3he4pmfx0v6fkxcqf5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzsag073","sign1e2pxran3l7fdqtft3s7sd4k4pgsfl50psz6yya79p4tlrc87nupp8a90tfpr3exek2a2rjdhtkpju8syfj8a6w4r2jaeghwdlrdk5qykdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpyygqur9","sign1vz8smjez9dhennj46t5awmz59m4mzynw2sfv649sa3mlk5hg6uqmcrmfzu5l5xjuwl9sl59qyx5scvnjjgyahp783qpgsnuy3xy6gqn7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6jterp5","sign1e2uy5qe6gpez2mftnfprn6uy9hxtendvpjnjdxvwjc6eh7e3zyqn5vndexwgwk8kzgeqnsnljtskl2hj9ue9mpka2uyg3fp8g2u6gqg9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkfwyqgv","sign145dzc3vvpmmxunzcuec8zjww8xdnhnsc63vzre5pn2hlxgtajupt6ur0nx5uzut7vf28jm8n87jmnqvt9u2dq55jxaxp8nk6zld8vqpl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjns4kl9","sign1uav5f3dctucy0gfkgrynvt25lkslvj89uszdts9h40g0v8vc5gqv43r337cnlnwz6xksvnutfege47sk9kc5g62ukj7t72265600kqw5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ssgyalq8"]},{"batch_header":{"batch_id":"6076376307338911630431676575048119160867781663955902961283067658097138666355field","author":"aleo1vfukg8ky2mhfprw63s0k0hl4vvd8573s6fkn8cv9y0ca6q27eq8qwdnxls","round":2414061,"timestamp":1729056561,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["25703415787984925685268205605672006314748098985693072579498043986596547774field","4376405578818296783364429436924610551956677457230964503352229688996235098853field","6933965430109437740988694168569137053083939200936117197609852926348021758181field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","3625194531782949063158791953110562455080234359211740435484165965284933251289field","7346183160846366786926355374150809034747622376304404229666720839678179964799field","6597932540108370401690242854116153078558657794717813742710488613461997241048field","3737034014389007601605510900786091865944626973329427365388630942952087232670field","1047693341435237257835045176414743568227351675449538425709853805002301314373field","384072313879710261533192845263462095097000139350805176007704655262055632600field"],"signature":"sign1jsxpja6kgzs2446gene99wm8z7zqqucc9u75354r6c0pz0zezupy08zj9xtm7c3ft3tmm3g9axf9eny3cyw07urzplpmzk786nakwqxhv57zdsct6n3aaz7wqe4aax79h62qd05zv60zwkctds3axy54qhpp70v5d09vtq7xk3uqtv9svq0clsc25dea08jtzkl868rkrresj8u53jd"},"signatures":["sign1wyqfajf4ev8ffuxshzs0a5n303ucv03r8cxnwjux45ygdevs0cq39tqdl5u7xlwjumtmh57tx6j0jerhg5xn3d2axl93ftreryz5sqetcugatxq5ged9snr7e264er8cts8f705myxk6cmzqwwa6d4k8q8hegqe5xtg8u3jn0pcf4yhnjzsskv7kqh6vs586pz6m67dj06lqka8a367","sign1l06wp6lf4p7zqep8nck2vt26nf505rljygdevvq5n3vk9h9druq44lsnq56azmjhq3k8ljwmqx0nhn2jdlxtt63n94w8794cygfvsq40mqecjdn66z9esugm3wfedeghsty3ymlk97u8rs8hgdzqzuh5puqnjedls3q957n00e5g0ahhmr48hvyzz6qa62daq2sga00nes5quuh4qwa","sign1gthp85rnh6appwxepnn0t0ml398dupv4f6kqwpyen0nwpeg4myprrwj8xcyhleqry2rg3nxe27k9rmmff04ph6mtcdcwvyjrl0uduqf5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzk3tljw","sign1geukv58pa48stnlag68p4l4vq6hjudyn7nfn8w2vhv450r9rzyqgyrld3s6yct2pq6kwt0n3rsmeesdgfkhc46r7huxe6g097kxxwqm7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6enllfg","sign10ap4zv7laygzhaktst5dwlh2eee5xunzxqwvrlvrmx8600wr7up2yc9q0pf7jwtf8zwm4s2cne5rl84vmegyr9gsy86sl4njyswl6q75g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467sshaf7dl","sign1mhpzkfqpd7cgvx6wne30syy9l7q69dgjk4df4amufcf5h3jfkvqckzpuvm29wyn23063e4u8qs585rlt3j8ly2c2xcv2q0drx02kcppl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqj92cwyu","sign1ae5v95ll3vmwlrcjzc88ep79nrv67d4nergm6g4dwvjqpw4yggqtsf6utk42c0nyxqh6630wur4agzm3tt4pg0paxd3vl8lpv2dd2qc9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkutu8q3","sign105jcvl6pxc3qunlpzqmmlym5k6t0gdv9s8kgpj7pypppxnhtfqqdl3gyw60x3d5f9an96mdg6675wp6qvmtlk6zjgz7dw572fm6d2qykdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpyftf7vd","sign1f89nsp327ug9vsnjhrcqqj8tvczah0fm2yuxltasw3cxeafrnvptaq3arqg93t943d6j37mhwvry6uf98mgzttue0jdphhs4c06tyq699xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcjndq2g","sign1kl549eac3z88d2727yq2rdgjrljrp6kvd90pfd6y3zep4fpkfcqa5pc9mk0vsyutahned6x9rtvccds6uc08ngsvhj265wer5pf6sqmvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2k0sray"]},{"batch_header":{"batch_id":"4209658276949563406813448967605338680606741202890217237210175031441106831000field","author":"aleo1hdtsgk52nsualvrt4t676m9sydp6zllwe0mr4w5mzknxj3rkzs8slhszhs","round":2414061,"timestamp":1729056561,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["7346183160846366786926355374150809034747622376304404229666720839678179964799field","6597932540108370401690242854116153078558657794717813742710488613461997241048field","3737034014389007601605510900786091865944626973329427365388630942952087232670field","384072313879710261533192845263462095097000139350805176007704655262055632600field","753955275865305033939063207991126376490947500486867809415281451294838738910field","3625194531782949063158791953110562455080234359211740435484165965284933251289field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","1047693341435237257835045176414743568227351675449538425709853805002301314373field","6933965430109437740988694168569137053083939200936117197609852926348021758181field","2833960716802695151265019157537143513285885873922032569490540653222759848157field","4376405578818296783364429436924610551956677457230964503352229688996235098853field","25703415787984925685268205605672006314748098985693072579498043986596547774field"],"signature":"sign1fv6l44f9my6phnt4qarfkuhsaf0md0kr08sqtjr8c72lydrdyuqp2fyszmhlz7xnkn02xscpl59hwvlxsr7n96my0ws8e560n4akvqy2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6hueg4n"},"signatures":["sign1t6fxsty3qks0237939vmk89ax4v9q8z5wh8jv4mm5y3t5xq4kgq8cawcu5yd3t2rprqtgwz6tmfgp565x4nfr2v7l3tenl7lrjkh7qvy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq52cxg78","sign1ep20c674xssdk2rrgspru94g25fjjg3sqphekypv5xh0m8jdavphl46e3j7yj9f5axt2hjwzyfpnqde6q0qrqsmvzckamtlmaw8eqq4nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2ewgxx3","sign1rxmsqqv4d3lv36xxmtn0wztpkev8ntdm26zzgw2xlqvm9se2mvq6wfwx8q40wjspxk6pqqahekyr8dak0qtxsh4gjlnh8n2p8cqq7pz99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqc5mrzxp","sign16huuk2w6wphnewll3zuk2ladfla5tqsvuvqpp49u72ddu4sawcpkjr37ac3kqvalyg89amvacn362e505kce5geyrfyxp0qttuch2qmvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s26l8h5q","sign16lcek3ts32eqs4crjh0yyml8vp3lwylv8zj0mtwy6y3226uwlgptmkjderk2xz5x05332j2qpnlyw85m4428hgg5zcsaq8cg0c54yql4fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5jp2jjr","sign1g7xnrgz446rfc9nd3vy0gtnyxcmnsd8y6nz6txyvja94kn7juuq6mu2jhkmgah7w94wue0lztsg3pd9wlr0eapqz9j6ah4znjcv76q3s2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs7ueydyk","sign1qr2h2tnqwegqvv9ycwaqhvt3kqrj7rmefpjd37epvjqygmhg5vphnt3j85sxrdcql3ssggdty23s7sx2jg5507k5aw55udyrqvkhcpp5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzyqk6u6","sign1xv8y8jkmmscu3g627gfja8en6j5xavy392z59ys440l2enh60gpt36623c02qhc332yvrla4yqh30jgjt5ydn0m7x38hccpzs0erqq5kdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpyjttuzr","sign1sdvwcddtgcml74l89g6hqve242s7yjmywffgwv7pzy2waylhvcqaazv9c0t5f6ukysmp43p2ds90vvgfsq2q90ptpgj84xvqyzl5yppl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjpnqqzg","sign1rlhsmz2gsrjjwmzw5v8ftxgfr80kvkwx79hzwsa4y0e047p2qqqh0uz4ak85vjqk22zr3w9lhpmglk9ufjh87309dlnr49rt5kkvgqt7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6r2ww8s","sign15eys672gmmv6zhnhjqf67s72ztp4nee7k8fpfgmrnjswrrm6mgqjmglk7sh7zrcw3nuzhvqcj4nr3ewdulfyu2nadnml8nyvtjtuxqq9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqk86vwp9","sign10te7ayaxgnxyuz63dpj4tavmvnm5hmlyjedr9lp6at5q2ypjs5peynlvpx6haxrmvskfypd6qcarq8nxyaf5fxah7sm6sgpqe352uqx5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ssvgp4su"]},{"batch_header":{"batch_id":"341493417958829703067989003985669220304142512556060502027385434865119931501field","author":"aleo107mzqjf3w2mw70wz0uy9r3u95y4s7af27jzxdyemz3sf9ase7v9ss6nr8t","round":2414061,"timestamp":1729056562,"committee_id":"5833279236581907707392504105601022220562007916066436974890418033047329926170field","transmission_ids":[],"previous_certificate_ids":["7346183160846366786926355374150809034747622376304404229666720839678179964799field","6597932540108370401690242854116153078558657794717813742710488613461997241048field","1047693341435237257835045176414743568227351675449538425709853805002301314373field","384072313879710261533192845263462095097000139350805176007704655262055632600field","3737034014389007601605510900786091865944626973329427365388630942952087232670field","3625194531782949063158791953110562455080234359211740435484165965284933251289field","753955275865305033939063207991126376490947500486867809415281451294838738910field","1232619116214198098204816232875560987377739846217396757086495058130192230615field","2833960716802695151265019157537143513285885873922032569490540653222759848157field","6933965430109437740988694168569137053083939200936117197609852926348021758181field","4376405578818296783364429436924610551956677457230964503352229688996235098853field","25703415787984925685268205605672006314748098985693072579498043986596547774field"],"signature":"sign1krxj39gmrzssrhmh288gdmvej7eyzczk23u8hl2v70vqzmyttvqh23nqzz8jp2w56q48p5krgett2u396v6d9zmyc493gzvff9xzcql4fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5hnns8k"},"signatures":["sign1ju2zlm0a5u09pejfdf7ny68ytgtrxe675e5grqc9nm6n3xdccuqrkgth0z598masg2c944t02y3fw9g9n9ulwvqe2nkdf00fn2x45qrvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s25tk37t","sign10zlndvj7rp44z32y0u6c67chja9gkuzusdf4ell3nt8ff7ga0cppsvthpedpeywvkcy6p6e03upfftt5r8esckxprkxp53xxkldrsqu2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs60adnmh","sign1wsg9hyp8xwf8fmzy0qwu3yxvmzqllx4wdja79gpaepdn2fcnpqp3fqxudu8a2pg8znw2v546pej346s3ajpywglm7x83quantpalzqfs2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs746vh4n","sign1j9g3nrrslxzd2eym7uqr2snfg4gjps244epxklccavdf6en87qq87mv8pzvku6yelc8z47e8sv5dx0n4pyj6fp3pchguemwtqm3cjq9nkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq2nvua5a","sign1hgyfskkfcdwuxlv4s0c7r5sgrmydn40ffmr2w4fls77ac5w85ypp2pgkagy4kttr7u8l8za78t5s3zdh64ttj8dvxh9j42zjw6t26q299xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcz6uphq","sign1unx0pcp8hmsl4tmp0q542pwyhmy0f358j0zts6pxsm87zlkr4qp8prlepmgcanj05400tavcq7zka8dajzqawj3qgk8q3qh2wp83jpyy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5fm9uke","sign1qy0am8v0qhj6ej6nt5h3q5ac28vve9nq0qrey0ye7eu3kfkxygp8mct8pnlvdyy8cv05a842lwd8cgk9ax9a8qmps2pn589t9qcz6qp5elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqz2rzq2m","sign1ku6fqsz0hw2f6f7ntc8ypjmynk5xskfgt38pmfg4h6qkaxwfssqxa7ql2j0eflu22r24ykyz4cyfknvkczatdchfjhz06cw86zymzqg9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkc029em","sign17t0f6n6qm6k95jrvf3v7g2qtynyfl5qstz3vka5j8qqcnphhfuppskz9q2yz9zk5zwyhjxljyks0wxy3ly2m00fuz0av9ypttfx02qm7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq60caa6g","sign1jdetx37y8h7uks8zyu5kuctzxkl27w272lgpj65satpxv2jh95pnnvhjarhs28te4xs6yz5nznm5uwk8h2gt2gadtrw3rc55eeecgq5kdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpy3kawld","sign1jxnj0t2dzjaf2znpdxy9s5686szxad5m9gg3gzt5e0n7tvuvacp6kyg90nta76xwt9yv9vu9fj9e6uf5pprtqhl27nhn3ucfz3g2jqxhv57zdsct6n3aaz7wqe4aax79h62qd05zv60zwkctds3axy54qhpp70v5d09vtq7xk3uqtv9svq0clsc25dea08jtzkl868rkrresjgclarr","sign1th95lmtgqq3q7cgjxrq34rcsfdv4lpq68hkxx5ymd4tvjz6n3cpv4qnrar22e7n8apfpme7clxfkjv465yjdkav5yxddws4pgw25gq3l4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjch4k8k"]}],"2414062":[{"batch_header":{"batch_id":"4570513216298968700032054115388049781497749658532037475200971923756464719801field","author":"aleo107mzqjf3w2mw70wz0uy9r3u95y4s7af27jzxdyemz3sf9ase7v9ss6nr8t","round":2414062,"timestamp":1729056564,"committee_id":"1400322021425153625979153891373693501800963502308470815621013568648489209608field","transmission_ids":["at1pmj3c89zuy82stwqhx6qgyxe28exazcdw7n22276wq4yzyufyuzq5eqyf3.20487699191180129766296701294130990522","solution1e0kql8lqngzcy3pm7jv.41669813030272759940837386722490422984"],"previous_certificate_ids":["1873332333030018133851183902550755179669888593491184483228605635677131740201field","5021599431286432159336755138493090315928326497197635232419490404070019335357field","658923340114891726635091301035053850382275420208453938502999474664931029264field","5787673979679948377579261798809042887560136347237249368768681591236899516585field","2355727057248138228337102165271013656898210569825776199748013689446439744475field","2673518712274515398354726341315183266197647227599002744248963819463701645925field","2409657726378783286112337974757762727034362624005334766411227987602260157485field","1147444120455701470930763712255746731365910729698800497927072817478339987039field","3308272729573063666725220787072185977982842256694541719108872762435055540697field","6076376307338911630431676575048119160867781663955902961283067658097138666355field","4209658276949563406813448967605338680606741202890217237210175031441106831000field","341493417958829703067989003985669220304142512556060502027385434865119931501field"],"signature":"sign1gka8seruxe5qgmv93meufhn8lk0kt27cdfdtlvdl9pp53aasvcqj6k2myug62rjeaylz49vad0gfgqg4c8yx2skl5zjmuj5md0kfvq04fcwcmnj0zz05jnhlmd6zayflr4jc7srmr9m3ujtvwl3erv5upfc63r33xqh7rlwqc6qejmysfzkdv4p4f29w9ythqtyxh0gd5p7s5cj5vqg"},"signatures":["sign1n74f0hgwr8c0ex6e3894a7vqkzjfwu53cs627lsd8hgsqmmpvcqjl4hhltyqgjt45jqlmth56frwtefknlw0c8ghcv0w34kjmswzuqyy52nuw2r7nd7jdsk2pmeydqf4nfkfasyt9sxedy8h7zs8xh7wquclutuw04cuh9q53ry0l00svkdhfzdqy49exfs7xxum9lvt8auq5fh28e9","sign14zw3yg5wyu78hj46sudrw4yw09e78dqp75zeu4acjq78mgthycq8hpg8lkzxknjzqt5za0ryvs83lpvd34tq7ywch5f3tlyjt28n6qnvmtdkzzkasmfv5u5phqqw9pmzq3e2f7u9s4gtcty037stt3mrzgjcevh2pu2vqq38a2g6m25jxvdqrzghkf4mval542dcjw9m3t6s2gaqquu","sign187tmwds3s9svm6d0js0k9rymx3tc0lynwjh4vgdevsrq2ujtnyqem7zzqhfz2zsulc5zx6z2szmqec9jjd8stx8gs2g2hann60s5kqdnkvgfn4xvgvtjjtfjjtr0ls2rz8fglg0tu2z9vef86x3e7xjppl3lw5xy5m78vsk2stegcttj6y2q4566w9ujpugz8ukx5guyx2yq29536h9","sign1ma2kzhenffflew4emcu4zry6wq7fj5r9dgdqef4gnvfaxz3s0spl5mfrku6k603yl99n9wj6tg0ffmdupu3976ty8cthcg7a2r2fgqj99xc6a4yecd7gd9anghntktjm5kslz3v9cxdq88n0zr8e33d8pkqgg7lfn4zrr8yl8yngejg330l5f7m9pcpele5ss7z0y6uqwwmqcnxuwms","sign160dmss22z3fdcflplp0zdnv4keswxl0pqlkk04rfaunqqcqecuqlj7scpnjsen3vg98myewecmcsnhpmt59qffe2805f8hsyguxfxqy2x7lxkevrrctxs538ghtjkqpe2sfugcylv0xfgqqdmxsxr0ezpqy2mx3h2m7hvzryf85pj0cm0txxqc3fsl75jh4hr3m06ftuqmxs6dw0ppy","sign1g430hn3d23fllslm2lwlajehag46gws4vfvqx6h73wsrwhcsauplj9qrenhul9j9ru6kju4yhed5hte6x75a2jjy4lhlf9x9fc0ljqfs2ma2lasp282wjgfdwsqcpf7urars3656kdfdnttha48kl5vgprwknhqyl3h257kq2zwzpf03vwug2t2f6h4j35getmz3xkls82hs7zrgsss","sign1zdw29vnaw2cw39g5nucyeez5u00qtfan9hay2ldfzsq3zyrr4gp79pvrs5c8yxknfx2m6lwdwkx9wd0ach7evskv5duhw04hq2ujuq35elgwmwwpkg2fzff5yup9s9rr7f474nkj4hy7av6apjshc8ztpmtxfedw2vh8wd8vuz45lwpvd94h30gxr5uk9hmxuv3uv4r0vtmqzkqhxjx","sign1dac9erc5xl3kmatvmsz4ekmxxrwjuaxvp4mfr8ajppxk7v9meyp08ct6aaqt67af87yur70v9dva4uzn94egqgk5jtdctk8zk5372qq9w3405uasx3czy2uzmtfgd8xglw5n02qr6nk5uepgex608eauqwgry8atyptv9tatvkanec8wwr0r4hm27fu3keyat4tnnxj8y5sqkw3e4xv","sign176zrlc9rhq6l2xmey5hmn8qthum6dt6k00k90e5k24sely4k2vprxqnw06hyltmya4f2a2feu5ltsvsz0zcfqzruq96l0cakqut6sqykdzy8ykmgs5szkvp04qas6c48dll92zxnevcav63xcrx4mmu8pgw4x96sslwel99xeyhz0nqjr92x48sqreqzgtqr0qfrctyr5ucpym2k0r8","sign1n5h6q740c2xet6rhg7gy578xhfef7dj5m9jqmx94y0zyfnuswspswsupxh28xkwl9x9cgndey0tqrcqrcs63cscl4k83ade0pujzsqr7denswjsfmmnn53rhlh7jjt8pnfkp8u398p32uk6y2r38adw8zxjdwvas6r59zustd4mjyyunemq6xc5sxjcu38ju34l9f2mv6zuq6fuaxj9","sign1ywwyw82j3aypl77qyeafxvfx3carw0va646hfztaxux46fvm6cphtjwvkrtcr0fds9k6qlakglhzdw334drklfj4qfwrvgsn05eg7ppl4ru33takd8sfff0lmlnnz8j6lwrj0u2aj3zzu3367yxqxk29qcc4n7hr6jll7rux5egupvwh7ahz3y54ap4k682y3q888nalzraqjvleens","sign14t04kal4zlw7f40hs50v0r2fdqcydryp7znkhgxpks0nn5nnkgq6q9wyjmhj7fc6nhuems8fyvgv8lup9y72gm5u077qphpmhakt7qw5g8lgjerrqtsnrktx9sjs2w2tg86cxp9gmsdhskg482den3empsag9fazjxfhmutvvwgvptch039m4ywpadv5dp7crjp0am82467ssf9q5jv"]}]}}},"ratifications":[{"type":"block_reward","amount":64993016},{"type":"puzzle_reward","amount":82421347}],"solutions":{"version":1,"solutions":{"solutions":[{"partial_solution":{"solution_id":"solution1e0kql8lqngzcy3pm7jv","epoch_hash":"ab1kvqdr67rfqnfnzae30k60u6sed7e434e6j73crxgj3l4kn0ljsgqtfuarx","address":"aleo1666y6x0qcxa7syyahys6tzalp3aqppqwj7tdf6purwtlyjkpwsxs3wtxlp","counter":12874387158213933576},"target":1676976733973595601}]}},"aborted_solution_ids":[],"transactions":[{"status":"accepted","type":"execute","index":0,"transaction":{"type":"execute","id":"at1pmj3c89zuy82stwqhx6qgyxe28exazcdw7n22276wq4yzyufyuzq5eqyf3","execution":{"transitions":[{"id":"au17ldqa7j8zq6zwqym34mrel23y87yw3n7ft5yhsrq3e662z0y3srqxq4tmp","program":"credits.aleo","function":"transfer_public","inputs":[{"type":"public","id":"2005670487040063231533321175737418906196511812257181776982552819091883069827field","value":"aleo1vs44yrqhmg77afudr43k2zeuqe724adc7p8ps0thjfy5e0wfksgsarj6pj"},{"type":"public","id":"4944472938488240876256194766420692344052326107853929396999165478727386757292field","value":"1550000u64"}],"outputs":[{"type":"future","id":"276923268234202210175855053678034751882995481102374080330249756461414560970field","value":"{\n  program_id: credits.aleo,\n  function_name: transfer_public,\n  arguments: [\n    aleo1mtfsa8hnhcr99v0n3m2ukayc60annk0kcaunf2ckhccav6lx55qsrhw0rl,\n    aleo1vs44yrqhmg77afudr43k2zeuqe724adc7p8ps0thjfy5e0wfksgsarj6pj,\n    1550000u64\n  ]\n}"}],"tpk":"4323145497615721671016770453461229260064095381468776741208847928620661155192group","tcm":"1085653006457515593240466155770523022776899979589539338323807229857489864102field","scm":"6512117559102085138850135240985425510584632932204325474711322762097143980382field"}],"global_state_root":"sr13ar6yw9vsqde06rplk57a7payxtt5q5p8920gltj04e7287w2qqs263c3a","proof":"proof1qyqsqqqqqqqqqqqpqqqqqqqqqqq8v7gjmsmxg27hy64tpw9m630vmvcykvgfjsq8gpsft8336jjvvc5x3secr25a7mz6a3tzjzrhzyqqq8qhxzq2f72pyqtwr3alyneca4mj7eyn2pt9wlqhxeslxe6l0p7lzn56q9gztr47pmtcqn5dfp2x9qzgczmnx7nejn2hpg469xdysgl864sazsqpt0rhxt5dnex2mr2j9kst6ephrdpv2rdmwuhs4cdq05qcvg22kjgpwv30r005en2svcdqv80l068hrpmrkv5tl98gzpzspycrccdn7s0gdr7sqcqlj0epe5cqlj254kju48sma0kyyhhax03j0e35udkykkyzu64qtwn2v5h43v3axvqe5sdjhgpv0f2l9yp2uz3cqkth64relcmhp9n4lczzehshcxtrxcm3x2csu30z4klwj8v6hy245gpae9dvux3ncrznaqngamjkqzlegsv7p7m9fmcna509djlwfqwgc5p8ggc5l9l4k5q0zr0h28a6chxr7h9nfcxsvhz3cgutyevgvqw2xs3l5ym5kkjp87gycmlek4ylq0d4surld3wtd4cv8t0pft34carrn3psk7r0qp5el7f32cwkwjq7d94gegql7v9dzfweve76re5hmvqa4vhgqp9ff7sx9tjggf9dttgue2ut3qevlvqlsj2v6e5nr6cqym7hmq42r9w8mgeftrr9u0lepp7c20f28n9mcz2h5n62tgz5gqxtu60jj3tz6fjvg5cgr9ayqphnur93dp3mdye6c0427702n9t7zyg3eqwkn9y2ctuu76cmhpa26ns8lqwfvl9ru0juyjcfrak003wkql5c7gjhdsqqjjk4trscxn3xt6whlzayu9wxdaduk626a8wcluysj6p6dj0rypk6ssf5vku94t5p448yfe0p7tfh90u2v4py4fw43ysd8e6acwrazyx55uud25djce2vakf8rf0x49v48mzvf34n3gu525xhyynjmtk8rz70q9mu9gs0hzwej2e2spcr87u9yxla2rg4tgsrspnfshkkjtwa8dpn7hhgc7wxj3lpmy7ru8pztg880farmhdwavukqx0nmq8p6lzcv6ysmkk5gcvths5z8dm9y2vzaet50yg9hlmjmp239hhev5w9s0yanyatxgeyl86gunpu0ke5rn4zlmmfkusadcytr7cqqvqqqqqqqqqqq2jk4myptg2pxcl8jm0zutmwdt9dp039qn4djhjl37fa3gvxdjg3532r2d39ep6ma0as3lcuvschqqq8r4fvprxjejlq7jal46mfee5c42lr7vzvjc6h2je0tt6aljveeaje6zrn7ag0flhq62uca29uacuqqygudv60fwelaugf3jvwg3nz24xlzfrljdxhmf24h0f8ahlmzceq026d2l7euquawn6lspkfvavcq2matw40grg4fzuvvwjkewy0lykyl42fpca6kc32nwktv6tyn69xqyqqmgnwg8"},"fee":{"transition":{"id":"au1t6mvx808tn4tpx804lt8hhd9rmel7el90zpy8fnplfy0dadzxufqlwutes","program":"credits.aleo","function":"fee_public","inputs":[{"type":"public","id":"7239119403488237213937712027790280349895700893418920487659938154572223810330field","value":"51060u64"},{"type":"public","id":"1354038555023992863158626557884025202644741608503055648817160137553689771044field","value":"0u64"},{"type":"public","id":"3124410131768714158083489581600542727999367311551933481297564788241420371184field","value":"1323469602572769137773173777465145518117575021180530994545910880487243654716field"}],"outputs":[{"type":"future","id":"5569832576887471132590316442196751418393557405363005082154930164124813066493field","value":"{\n  program_id: credits.aleo,\n  function_name: fee_public,\n  arguments: [\n    aleo1mtfsa8hnhcr99v0n3m2ukayc60annk0kcaunf2ckhccav6lx55qsrhw0rl,\n    51060u64\n  ]\n}"}],"tpk":"6318122497769794687729048903817690477412114521597192495072085836255731943970group","tcm":"2163143388946502973101028062796960004559702799012890181179540269729093029007field","scm":"3564844773917395681503328722571379222728087448161524817262294083550517171411field"},"global_state_root":"sr13ar6yw9vsqde06rplk57a7payxtt5q5p8920gltj04e7287w2qqs263c3a","proof":"proof1qyqsqqqqqqqqqqqpqqqqqqqqqqqzh7ppsfm9u5rnlvctp2q4djf8jfjwslpnwlgwj4d7tmadp97tzk0lael7kxq4z9ltrtc2eh3am7yqq98fqrc8g37tn46rgfr35jd23p0k29d4wm8dll2yfq6etg578jup65hv86t60ma4azj2d9ercuv35q8tr5xs2rjarwnpl8g9c49kg32cw0v30qdn3lmhjzml0vvm07jprp66thtsk4g29vx03xla3tcdd7qcapdgte3k3prlrd8c7fqqzxuw553dg60a2fletu6clc622m0nypz6z5wxstte5dgtfj6ka5cmz4vppt9jczg8uax0z3y6m8eap064kyqp3cpmcxkwv26yjgd2x5zlp6lkjjms2wh5lyujjgeqlvr4nw0qr503rez2ky6ylwpxsa3dp5sfjs7dn6jdd6vfwpp7vphvyju99wwq62t6dexgaz36h9s89ql52sgyqqunw9fvqacxdc0uw2nkqys9z9yx9u620zdlzddgsenxsz5ajcprgf8jf8nqfjy2ek86qgmyl976aqxsmm9fkzn0njw8ly5kpnfeenuwhf5h9tn0rzh0mf5qnrja6kqnlz4aks45kdgac7lpda33jn6csuq8kk2af2u3u4swh954pdc05awgdr47n2rgaxxx5mppd0nrfhkzfk59u7ffy4gx0as6z7v2dlujx8vpnwgzvxe3js0us5j5m7csfm4dgt5tls8n2pxy22swp8dkkymenqg3xhqhfu4g5cq2yqzyc92es4cswy60wxrvfxnsxukmhr690vrr6z9nmtftwutpk5rqm5kqvv5gtfmhs0qfld63jgw7c2fhlvkuusmxpcfsg63pr5chww85xqvw6kvhfe7v5ttza60wmpey7ktxnv5rswfswpam83kptkh2z9499sgsctt5wg9vl9ljt5e4x7v2ctuzv2aetqsf90x5wu4dnd4jdhgdjq2hngpc38ytwf9wv2k9t2xdt4tu0tgc0spvx4mte5uwza8pq3pq4pnscpkr3y7j3euwal7ercau68v2cxatvqse97y2pqnh944zzjk57pts37y0qttu4g0ufelzx8py8mwvnq7spuequmgpjjan6vl3yfyyl7vhg99jcfvfcs2cm5kzs8uhh3dlem2qxx8xfxltadlqqsz09mfzgrtzz8wc2u9lt3sfmnw45w5kkqurzfg3qvqqqqqqqqqqq9j4mdfuhf4h5gke2whmtxuw9exq8c2j0nnz24tjf8ku7cp4n9ew76jfcpzasxeeluwmhkgar0nnqyqzh3skgdakadrtm0t07h66pmn2mveejmlf3rrr8ktwjlhlh3x2wknxc2p7csjd6kg5uyzeedu6wcvpq8q8x03kx0rvqdwhrxswyh3uqge7e3lxu3naarmruslhrp2jzgus60vsfvn8znvs0zzez8zndm8xf965ce9zwycn2nkaf06hxn037z8mhfqvq00hx84tazk5w3d63sqnqyqqy2g09g"}},"finalize":[{"type":"update_key_value","mapping_id":"2855157744830843716005407030207142101853521493742120919939436395872133863104field","key_id":"1729557898781947339937644362577060241965000121578373030252706367662374147464field","value_id":"6011413600727769955196139618632698687615039512527026886368957511420779837142field"},{"type":"update_key_value","mapping_id":"2855157744830843716005407030207142101853521493742120919939436395872133863104field","key_id":"3060720147028030884278053875486642783993102225473535487097000706809680912626field","value_id":"7242300325724977938138683324501323131697954017568834419075332156531889278165field"},{"type":"update_key_value","mapping_id":"2855157744830843716005407030207142101853521493742120919939436395872133863104field","key_id":"1729557898781947339937644362577060241965000121578373030252706367662374147464field","value_id":"3876781478221386607748111529789217937667524239752708693049340884444900344414field"}]}],"aborted_transaction_ids":[]}"#;

        let json_data2 = preprocess_json(json_data);
        let block: Block<MainnetV0> = serde_json::from_str::<Block<MainnetV0>>(&json_data2).unwrap();

        println!("{}", block.header().metadata().cumulative_weight());

        let json_data = format!("[{}, {}]", json_data, json_data);
        let json_data2 = preprocess_json(&json_data);
        let blocks: Vec<Block<MainnetV0>> = serde_json::from_str::<Vec<Block<MainnetV0>>>(&json_data2).unwrap();

        println!("{}", blocks[0].header().metadata().cumulative_weight());
    }
}