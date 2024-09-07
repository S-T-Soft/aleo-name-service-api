use std::{env, fmt};
use std::error::Error;
use std::str::FromStr;
use std::time::Duration;
use std::sync::{Arc, Mutex};
use lazy_static::lazy_static;
use tokio::time::sleep;
use snarkvm_console_network::Network;
use snarkvm_ledger_block::{Block, Transaction};
use snarkvm_console_network::prelude::ToBytes;
use snarkvm_console_program::{Field, Address, Argument, FromBytes};
use snarkvm_ledger_block::{Transition};
use tokio_postgres::NoTls;
use tracing::{error, info, warn};
use crate::{client, utils};

static MAX_BLOCK_RANGE: u32 = 50;
const CDN_ENDPOINT: &str = "https://s3.us-west-1.amazonaws.com/testnet.blocks/phase3";
const ANS_BLOCK_HEIGHT_START: i64 = 1;

#[derive(Debug)]
struct IndexError(Box<dyn Error>);

lazy_static! {
    static ref PROGRAM_ID: &'static str = "aleo_name_service_registry";
    static ref REGISTER: &'static str = "register";
    static ref REGISTER_TLD: &'static str = "register_tld";
    static ref REGISTER_PRIVATE: &'static str = "register_private";
    static ref REGISTER_PUBLIC: &'static str = "register_public";
    static ref TRANSFER_PUBLIC: &'static str = "transfer_public";
    static ref CONVERT_PRIVATE_TO_PUBLIC: &'static str = "transfer_private_to_public";
    static ref CONVERT_PUBLIC_TO_PRIVATE: &'static str = "transfer_public_to_private";
    static ref TRANSFER_FROM_PUBLIC: &'static str = "transfer_from_public";
    static ref SET_PRIMARY_NAME: &'static str = "set_primary_name";
    static ref UNSET_PRIMARY_NAME: &'static str = "unset_primary_name";
    static ref SET_RESOLVER: &'static str = "set_resolver";
    static ref BURN: &'static str = "burn";

    static ref RECORD_PROGRAM_ID: &'static str = "ans_resolver";
    static ref SET_RESOLVER_RECORD: &'static str = "set_resolver_record";
    static ref UNSET_RESOLVER_RECORD: &'static str = "unset_resolver_record";

    static ref TRANSFER_PROGRAM_ID: &'static str = "ans_credit_transfer";
    static ref TRANSFER_CREDITS: &'static str = "transfer_credits";
    static ref CLAIM_CREDITS_PUBLIC: &'static str = "claim_credits_public";
    static ref CLAIM_CREDITS_PRIVATE: &'static str = "claim_credits_private";

    static ref DB_POOL: deadpool_postgres::Pool = {
        let db_url = env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://casaos:casaos@10.0.0.17:5432/aleoe".to_string());
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
                        if let Ok(blocks) = serde_json::from_value::<Vec<Block<N>>>(response) {
                            for data in blocks {
                                index_data(&data).await;
                            }
                        } else {
                            warn!("err parse response!");
                        }
                    },
                    Err(e) => eprintln!("Error fetching data: {}", e),
                }

            } else {
                match client::get_block(block_number as u32).await {
                    Ok(response) => {
                        if let Ok(data) = serde_json::from_value::<Block<N>>(response) {
                            index_data(&data).await;
                        } else {
                            warn!("err parse response!");
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
    let mut local_latest_height = ANS_BLOCK_HEIGHT_START;
    let db_client = DB_POOL.get().await?;
    let db_schema = env::var("DB_SCHEMA").unwrap_or_else(|_| "ansb".to_string());
    db_client.execute(format!("SET search_path TO {db_schema}").as_str(), &[]).await.unwrap();

    let query = "select height from block order by height desc limit 1";
    let query = db_client.prepare(&query).await.unwrap();
    let rows = db_client.query(&query, &[]).await?;
    if !rows.is_empty() {
        local_latest_height = rows.get(0).unwrap().get(0);
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
                        name if name == *CONVERT_PRIVATE_TO_PUBLIC => convert_private_to_public(&db_trans, &block, &transaction, transition).await,
                        name if name == *CONVERT_PUBLIC_TO_PRIVATE => convert_public_to_private(&db_trans, &block, &transaction, transition).await,
                        name if name == *TRANSFER_FROM_PUBLIC => transfer_from_public(&db_trans, &block, &transaction, transition).await,
                        name if name == *TRANSFER_PUBLIC => transfer_public(&db_trans, &block, &transaction, transition).await,
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

        db_trans.execute("INSERT INTO ans_name (name_hash, transfer_key, name, parent, resolver, full_name, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5, $6, $7, $8, $9) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &transfer_key, &name, &parent, &resolver, &full_name, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
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

        db_trans.execute("INSERT INTO ans_name (name_hash, transfer_key, name, parent, resolver, full_name, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5, $6, $7, $8, $9) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &transfer_key, &name, &"0field".to_string(), &"".to_string(), &name, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
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

async fn convert_private_to_public<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
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

async fn convert_public_to_private<N: Network>(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(0).unwrap();
        let name_hash_arg = args.get(1).unwrap();

        let owner: String = parse_address(owner_arg).unwrap();
        let name_hash: String = parse_field(name_hash_arg).unwrap();

        db_trans.execute("DELETE from ans_nft_owner WHERE name_hash=$1", &[&name_hash]).await.unwrap();

        let version = 2;
        db_trans.execute("INSERT INTO ans_name_version (name_hash, version, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET version = ans_name_version.version + 1, block_height=$3, transaction_id=$4, transition_id=$5",
                         &[&name_hash, &version, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();
        db_trans.execute("DELETE from ans_primary_name WHERE name_hash=$1 AND address=$2", &[&name_hash, &owner]).await.unwrap();

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

        let version = 2;
        db_trans.execute("INSERT INTO ans_name_version (name_hash, version, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET version = ans_name_version.version + 1, block_height=$3, transaction_id=$4, transition_id=$5",
                         &[&name_hash, &version, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();
        db_trans.execute("DELETE from ans_primary_name WHERE name_hash=$1 AND address=$2", &[&name_hash, &owner]).await.unwrap();

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

        let version = 2;
        db_trans.execute("INSERT INTO ans_name_version (name_hash, version, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET version = ans_name_version.version + 1, block_height=$3, transaction_id=$4, transition_id=$5",
                         &[&name_hash, &version, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();
        db_trans.execute("DELETE from ans_primary_name WHERE name_hash=$1 AND address=$2", &[&name_hash, &owner]).await.unwrap();

        info!(">> transfer_public {} {} caller {}", name_hash, owner, caller)
    } else {
        error!(">> transfer_public: Error in {} | {}", block.height(), transaction.id())
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
        let owner_arg = args.get(1).unwrap();
        let category_arg = args.get(2).unwrap();
        let content_arg = args.get(3).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let owner = parse_address(owner_arg).unwrap();
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

        info!("set_resolver_record: {} {} {} {} {}", owner, name_hash, category, content, version)
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
        let owner_arg = args.get(1).unwrap();
        let category_arg = args.get(2).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let owner = parse_address(owner_arg).unwrap();
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

        info!("unset_resolver_record: {} {} {} {}", owner, name_hash, category, version)
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

// generate tests
#[cfg(test)]
mod tests {
    use snarkvm_console_program::{Plaintext, Value};
    use snarkvm_console_network::{FromFields, ToFields};
    use super::*;

    #[test]
    fn test_parse_u64<N: Network>() {
        let u64_val = Value::<N>::from_str("100u64").unwrap();
        let u64_arg = Argument::<N>::Plaintext(Plaintext::<N>::from_fields(&u64_val.to_fields().unwrap()).unwrap());
        let result = parse_u64(&u64_arg);
        assert_eq!(result, Ok(100));
    }

    #[test]
    fn test_parse_address<N: Network>() {
        let address_val = Value::<N>::from_str("aleo1q6qstg8q8shwqf5m6q5fcenuwsdqsvp4hhsgfnx5chzjm3secyzqt9mxm8").unwrap();
        let address_arg = Argument::<N>::Plaintext(Plaintext::<N>::from_fields(&address_val.to_fields().unwrap()).unwrap());
        let result = parse_address(&address_arg);
        assert_eq!(result, Ok("aleo1q6qstg8q8shwqf5m6q5fcenuwsdqsvp4hhsgfnx5chzjm3secyzqt9mxm8".to_string()));
    }
}