// indexer.rs

use std::{env, fmt};
use std::error::Error;
use std::time::Duration;
use std::sync::{Arc, Mutex};
use futures::stream::{self, StreamExt};
use lazy_static::lazy_static;
use tokio::time::sleep;
use snarkvm_console_network::{FromBits, Testnet3, ToBits};
use snarkvm_ledger_block::{Block, Transaction};
use reqwest;
use snarkvm_console_network::prelude::ToBytes;
use snarkvm_console_program::{Field, Address, Identifier, FromField, Argument, FromBytes};
use snarkvm_ledger_block::{Transition};
use tokio_postgres::{Client, NoTls};

type N = Testnet3;
static MAX_BLOCK_RANGE: u32 = 50;
const CDN_ENDPOINT: &str = "https://s3.us-west-1.amazonaws.com/testnet3.blocks/phase3";
const DEFAULT_API_PRE: &str = "https://api.explorer.aleo.org";
const ANS_BLOCK_HEIGHT_START: i64 = 649183;

lazy_static! {
    static ref PROGRAM_ID_FIELD: Field<N> = Field::<N>::from_bits_le(&"aleo_name_service_registry_v3".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref PROGRAM_ID: Identifier<N> = Identifier::<N>::from_field(&*PROGRAM_ID_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref REGISTER_FIELD: Field<N> = Field::<N>::from_bits_le(&"register".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref REGISTER: Identifier<N> = Identifier::<N>::from_field(&*REGISTER_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref REGISTER_TLD_FIELD: Field<N> = Field::<N>::from_bits_le(&"register_tld".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref REGISTER_TLD: Identifier<N> = Identifier::<N>::from_field(&*REGISTER_TLD_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref REGISTER_PRIVATE_FIELD: Field<N> = Field::<N>::from_bits_le(&"register_private".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref REGISTER_PRIVATE: Identifier<N> = Identifier::<N>::from_field(&*REGISTER_PRIVATE_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref REGISTER_PUBLIC_FIELD: Field<N> = Field::<N>::from_bits_le(&"register_public".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref REGISTER_PUBLIC: Identifier<N> = Identifier::<N>::from_field(&*REGISTER_PUBLIC_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref TRANSFER_PUBLIC_FIELD: Field<N> = Field::<N>::from_bits_le(&"transfer_public".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref TRANSFER_PUBLIC: Identifier<N> = Identifier::<N>::from_field(&*TRANSFER_PUBLIC_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref CONVERT_PUBLIC_FIELD: Field<N> = Field::<N>::from_bits_le(&"convert_private_to_public".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref CONVERT_PRIVATE_TO_PUBLIC: Identifier<N> = Identifier::<N>::from_field(&*CONVERT_PUBLIC_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref CONVERT_PRIVATE_FIELD: Field<N> = Field::<N>::from_bits_le(&"convert_public_to_private".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref CONVERT_PUBLIC_TO_PRIVATE: Identifier<N> = Identifier::<N>::from_field(&*CONVERT_PRIVATE_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref SET_PRIMARY_NAME_FIELD: Field<N> = Field::<N>::from_bits_le(&"set_primary_name".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref SET_PRIMARY_NAME: Identifier<N> = Identifier::<N>::from_field(&*SET_PRIMARY_NAME_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref UNSET_PRIMARY_NAME_FIELD: Field<N> = Field::<N>::from_bits_le(&"unset_primary_name".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref UNSET_PRIMARY_NAME: Identifier<N> = Identifier::<N>::from_field(&*UNSET_PRIMARY_NAME_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref SET_RESOLVER_FIELD: Field<N> = Field::<N>::from_bits_le(&"set_resolver".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref SET_RESOLVER: Identifier<N> = Identifier::<N>::from_field(&*SET_RESOLVER_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref SET_RESOLVER_RECORD_FIELD: Field<N> = Field::<N>::from_bits_le(&"set_resolver_record".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref SET_RESOLVER_RECORD: Identifier<N> = Identifier::<N>::from_field(&*SET_RESOLVER_RECORD_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref UNSET_RESOLVER_RECORD_FIELD: Field<N> = Field::<N>::from_bits_le(&"unset_resolver_record".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref UNSET_RESOLVER_RECORD: Identifier<N> = Identifier::<N>::from_field(&*UNSET_RESOLVER_RECORD_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref CLEAR_RESOLVER_RECORD_FIELD: Field<N> = Field::<N>::from_bits_le(&"clear_resolver_record".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref CLEAR_RESOLVER_RECORD: Identifier<N> = Identifier::<N>::from_field(&*CLEAR_RESOLVER_RECORD_FIELD)
        .expect("Failed to create Identifier from Field");
    static ref BURN_FIELD: Field<N> = Field::<N>::from_bits_le(&"burn".as_bytes().to_bits_le())
        .expect("Failed to create Field from bits");
    static ref BURN: Identifier<N> = Identifier::<N>::from_field(&*BURN_FIELD)
        .expect("Failed to create Identifier from Field");
}

#[derive(Debug)]
struct IndexError(Box<dyn Error>);

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "IndexError Error: {}", self.0)
    }
}

impl Error for IndexError {}

pub async fn sync_data() {
    let latest_height = get_latest_height().await.unwrap();
    match sync_from_cdn(latest_height).await {
        Ok(_) => println!("sync from cdn finished!"),
        _ => {}
    }

    loop {
        let mut db_client = connect_db().await.unwrap();
        let block_number = get_next_block_number(latest_height, &db_client).await.unwrap_or_else(|e| {
            eprintln!("Error fetching next block number: {}", e);
            -1
        });

        if block_number > -1 {
            let url_pre = env::var("API_PREFIX").unwrap_or_else(|_| DEFAULT_API_PRE.to_string());
            let url = format!("{}/v1/testnet3/block/{}", url_pre, block_number);

            match reqwest::get(&url).await {
                Ok(response) => {
                    if let Ok(data) = response.json::<Block<N>>().await {
                        index_data(&data, &mut db_client).await;
                    }
                },
                Err(e) => eprintln!("Error fetching data: {}", e),
            }

            sleep(Duration::from_micros(50)).await;
        } else {
            sleep(Duration::from_secs(5)).await;
        }
    }
}


async fn sync_from_cdn(init_latest_height: u32) -> Result<(), Box<dyn Error>> {
    let mut db_client = connect_db().await.unwrap();
    let block_number = match get_next_block_number(init_latest_height, &db_client).await {
        Ok(number) => number,
        Err(e) => {
            return Err(Box::new(IndexError(e)));
        }
    };

    // local block height
    let start = block_number as u32;
    let end = init_latest_height;
    let total_blocks = end.saturating_sub(start);

    println!("Sync {total_blocks} blocks from CDN (0% complete)...");

    let mut current_start = start;
    let batch_size = 1000u32;

    while current_start < end {
        let current_end = std::cmp::min(current_start + batch_size, end);

        let cdn_request_start = current_start.saturating_sub(current_start % MAX_BLOCK_RANGE);
        let cdn_request_end = current_end.saturating_sub(current_end % MAX_BLOCK_RANGE);

        let blocks_to_process = Arc::new(Mutex::new(Vec::new()));
        let blocks_to_process_clone = blocks_to_process.clone();

        // Scan the blocks via the CDN.
        let _ = snarkos_node_cdn::load_blocks(
            &CDN_ENDPOINT,
            cdn_request_start,
            Some(cdn_request_end),
            move |block| {
                if block.height() >= start && block.height() <= end {
                    let mut blocks = blocks_to_process_clone.lock().unwrap();
                    blocks.push(block);
                }
                Ok(())
            },
        ).await;

        let blocks = blocks_to_process.lock().unwrap().clone();
        let mut block_stream = stream::iter(blocks);
        while let Some(block) = block_stream.next().await {
            let percentage_complete =
                block.height().saturating_sub(start) as f64 * 100.0 / total_blocks as f64;
            println!("Sync {total_blocks} blocks from CDN ({percentage_complete:.2}% complete)...");
            index_data(&block, &mut db_client).await;
        }

        current_start = current_end + 1;
    }

    Ok(())
}

async fn get_latest_height() -> Result<u32, Box<dyn Error>> {
    let url_pre = env::var("API_PREFIX").unwrap_or_else(|_| DEFAULT_API_PRE.to_string());
    let url = format!("{}/v1/testnet3/block/height/latest", url_pre);
    loop {
        match reqwest::get(&url).await {
            Ok(response) => {
                let body = response.text().await?;
                return Ok(body.parse::<u32>()?);
            }
            Err(err) => {
                eprintln!("Error: {}", err);
                sleep(Duration::from_secs(5)).await;
            }
        }
    }

}

async fn get_next_block_number(init_latest_height: u32, db_client: &Client) -> Result<i64, Box<dyn Error>> {
    let mut local_latest_height = ANS_BLOCK_HEIGHT_START;

    let query = "select height from ans3.block order by height desc limit 1";
    let query = db_client.prepare(&query).await.unwrap();
    let rows = db_client.query(&query, &[]).await?;
    if !rows.is_empty() {
        local_latest_height = rows.get(0).unwrap().get(0);
    }

    let mut latest_height= init_latest_height;
    if local_latest_height >= init_latest_height as i64 {
        latest_height = get_latest_height().await?;
    }

    println!("Latest height: {}", latest_height);

    let height = if latest_height as i64 > local_latest_height {
        local_latest_height + 1
    } else {
        -1
    };
    Ok(height)
}

async fn connect_db() -> Result<Client, tokio_postgres::Error> {
    let conn_str = env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql://casaos:casaos@10.0.0.17:5432/aleoe".to_string());

    loop {
        match tokio_postgres::connect(&conn_str, NoTls).await {
            Ok((client, connection)) => {
                tokio::spawn(async move {
                    if let Err(e) = connection.await {
                        eprintln!("connection db err: {}", e)
                    }
                });
                return Ok(client);
            }
            Err(err) => {
                eprintln!("Error connecting to the database: {}", err);
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn index_data(block: &Block<N>, mut db_client: &mut Client) {
    println!("Process block {} on {}", block.height(), block.timestamp());
    let db_trans = db_client.transaction().await.unwrap();

    db_trans.execute("INSERT INTO ans3.block (height, block_hash, previous_hash, timestamp) VALUES ($1, $2,$3, $4) ON CONFLICT (height) DO NOTHING",
                     &[&(block.height() as i64), &block.hash().to_string(), &block.previous_hash().to_string(), &block.timestamp()]).await.unwrap();

    for transaction in block.transactions().clone().into_iter() {
        if transaction.is_accepted() {
            for transition in transaction.transitions() {
                if transition.program_id().name() == &*PROGRAM_ID {
                    println!("> process transition {}, function name: {}",
                             transition.id(), transition.function_name());
                    match transition.function_name() {
                        name if name == &*REGISTER => register(&db_trans, &block, &transaction, transition).await,
                        name if name == &*REGISTER_TLD => register_tld(&db_trans, &block, &transaction, transition).await,
                        name if name == &*REGISTER_PRIVATE => register(&db_trans, &block, &transaction, transition).await,
                        name if name == &*REGISTER_PUBLIC => register(&db_trans, &block, &transaction, transition).await,
                        name if name == &*CONVERT_PRIVATE_TO_PUBLIC => convert_private_to_public(&db_trans, &block, &transaction, transition).await,
                        name if name == &*CONVERT_PUBLIC_TO_PRIVATE => convert_public_to_private(&db_trans, &block, &transaction, transition).await,
                        name if name == &*TRANSFER_PUBLIC => transfer_public(&db_trans, &block, &transaction, transition).await,
                        name if name == &*SET_PRIMARY_NAME => set_primary_name(&db_trans, &block, &transaction, transition).await,
                        name if name == &*UNSET_PRIMARY_NAME => unset_primary_name(&db_trans, &block, &transaction, transition).await,
                        name if name == &*SET_RESOLVER => set_resolver(&db_trans, &block, &transaction, transition).await,
                        name if name == &*SET_RESOLVER_RECORD => set_resolver_record(&db_trans, &block, &transaction, transition).await,
                        name if name == &*UNSET_RESOLVER_RECORD => unset_resolver_record(&db_trans, &block, &transaction, transition).await,
                        name if name == &*CLEAR_RESOLVER_RECORD => clear_resolver_record(&db_trans, &block, &transaction, transition).await,
                        name if name == &*BURN => burn(&db_trans, &block, &transaction, transition).await,
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
async fn register(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
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
        let mut full_name = name.clone();

        let query = "SELECT full_name FROM ans3.ans_name WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await.unwrap();
        let rows = db_trans.query(&query, &[&parent]).await.unwrap();
        if !rows.is_empty() {
            let parent_full_name = rows.get(0).unwrap().get(0);
            full_name = name.clone() + &".".to_string() + parent_full_name;
        }

        db_trans.execute("INSERT INTO ans3.ans_name (name_hash, name, parent, resolver, full_name, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5, $6, $7, $8) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &name, &parent, &resolver, &full_name, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        println!(">> register: {} {} {} {} {}", name, parent, name_hash, full_name, resolver)
    } else {
        println!(">> register: Error")
    };

}

async fn register_tld(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let hash_caller_arg = args.get(0).unwrap();
        let registrar_arg = args.get(1).unwrap();
        let name_hash_arg = args.get(2).unwrap();
        let name_arg = args.get(3).unwrap();

        let registrar: String = parse_address(registrar_arg).unwrap();
        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let name = parse_str_4u128(name_arg).unwrap();

        db_trans.execute("INSERT INTO ans3.ans_name (name_hash, name, parent, resolver, full_name, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5, $6, $7, $8) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &name, &"0field".to_string(), &"".to_string(), &name, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();
        db_trans.execute("INSERT INTO ans3.ans_nft_owner (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &registrar, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        println!(">> register_tld {} {} {}", name_hash, name, registrar)
    } else {
        println!(">> register_tld: Error")
    };

}

async fn convert_private_to_public(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(0).unwrap();
        let name_hash_arg = args.get(1).unwrap();

        let owner: String = parse_address(owner_arg).unwrap();
        let name_hash: String = parse_field(name_hash_arg).unwrap();

        db_trans.execute("INSERT INTO ans3.ans_nft_owner (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO NOTHING",
                         &[&name_hash, &owner, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        println!(">> convert_private_to_public {} {}", name_hash, owner)
    } else {
        println!(">> convert_private_to_public: Error")
    };
}

async fn convert_public_to_private(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(0).unwrap();
        let name_hash_arg = args.get(1).unwrap();

        let owner: String = parse_address(owner_arg).unwrap();
        let name_hash: String = parse_field(name_hash_arg).unwrap();

        db_trans.execute("DELETE from ans3.ans_nft_owner WHERE name_hash=$1", &[&name_hash]).await.unwrap();

        let version = 2;
        db_trans.execute("INSERT INTO ans3.ans_name_version (name_hash, version, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET version = ans3.ans_name_version.version + 1, block_height=$3, transaction_id=$4, transition_id=$5",
                         &[&name_hash, &version, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();
        db_trans.execute("DELETE from ans3.ans_primary_name WHERE name_hash=$1 AND address=$2", &[&name_hash, &owner]).await.unwrap();

        println!(">> convert_public_to_private {} {}", name_hash, owner)
    } else {
        println!(">> convert_public_to_private: Error")
    };
}

async fn transfer_public(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
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

        let query = "SELECT address FROM ans3.ans_nft_owner WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await.unwrap();
        let rows = db_trans.query(&query, &[&name_hash]).await.unwrap();

        let owner:String = rows.get(0).unwrap().get(0);


        db_trans.execute("INSERT INTO ans3.ans_nft_owner (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET address = $2, block_height=$3, transaction_id=$4, transition_id=$5 ",
                         &[&name_hash, &receiver, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        let version = 2;
        db_trans.execute("INSERT INTO ans3.ans_name_version (name_hash, version, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET version = ans3.ans_name_version.version + 1, block_height=$3, transaction_id=$4, transition_id=$5",
                         &[&name_hash, &version, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();
        db_trans.execute("DELETE from ans3.ans_primary_name WHERE name_hash=$1 AND address=$2", &[&name_hash, &owner]).await.unwrap();

        println!(">> transfer_public {} {} caller {}", name_hash, owner, caller)
    } else {
        println!(">> transfer_public: Error")
    };
}

async fn set_primary_name(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();

        let name_hash_arg = args.get(0).unwrap();
        let owner_arg = args.get(1).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let owner: String = parse_address(owner_arg).unwrap();

        db_trans.execute("INSERT INTO ans3.ans_primary_name (name_hash, address, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (address) DO UPDATE SET address = $2, block_height=$3, transaction_id=$4, transition_id=$5 ",
                         &[&name_hash, &owner, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        println!(">> set_primary_name {} {}", name_hash, owner)
    } else {
        println!(">> set_primary_name: Error")
    };
}

async fn unset_primary_name(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let owner_arg = args.get(0).unwrap();

        let owner: String = parse_address(owner_arg).unwrap();

        db_trans.execute("DELETE from ans3.ans_primary_name WHERE address=$1", &[&owner]).await.unwrap();

        println!(">> unset_primary_name {}", owner)
    } else {
        println!(">> unset_primary_name: Error")
    };
}

async fn set_resolver(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
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

        db_trans.execute("UPDATE ans3.ans_name set resolver=$1  WHERE name_hash=$2 ",
                         &[&resolver, &name_hash]
        ).await.unwrap();

        println!(">> set_resolver {} {}", name_hash, owner)
    } else {
        println!(">> set_resolver: Error")
    };
}

async fn set_resolver_record(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
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
        let query = "SELECT version FROM ans3.ans_name_version WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await.unwrap();
        let rows = db_trans.query(&query, &[&name_hash]).await.unwrap();
        if !rows.is_empty() {
            version = rows.get(0).unwrap().get(0);
        }

        db_trans.execute("INSERT INTO ans3.ans_resolver (name_hash, category, version, name, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5, $6, $7) ON CONFLICT (name_hash, category, version) DO UPDATE SET name=$4, block_height=$5, transaction_id=$6, transition_id=$7",
                         &[&name_hash, &category, &version, &content, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        println!(">> set_resolver_record: {} {} {} {} {}", owner, name_hash, category, content, version)
    } else {
        println!(">> set_resolver_record: Error")
    };

}

async fn unset_resolver_record(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
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
        let query = "SELECT version FROM ans3.ans_name_version WHERE name_hash=$1 limit 1";
        let query = db_trans.prepare(&query).await.unwrap();
        let rows = db_trans.query(&query, &[&name_hash]).await.unwrap();
        if !rows.is_empty() {
            version = rows.get(0).unwrap().get(0);
        }

        db_trans.execute("DELETE from ans3.ans_resolver where name_hash=$1 and category=$2 and version=$3 ",
                         &[&name_hash, &category, &version]
        ).await.unwrap();

        println!(">> unset_resolver_record: {} {} {} {}", owner, name_hash, category, version)
    } else {
        println!(">> set_resolver_record: Error")
    };
}

async fn clear_resolver_record(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).unwrap();
        let owner_arg = args.get(1).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();
        let owner = parse_address(owner_arg).unwrap();

        let version = 1;
        db_trans.execute("INSERT INTO ans3.ans_name_version (name_hash, version, block_height, transaction_id, transition_id) \
                                    VALUES ($1, $2,$3, $4, $5) ON CONFLICT (name_hash) DO UPDATE SET version = ans3.ans_name_version.version + 1, block_height=$3, transaction_id=$4, transition_id=$5",
                         &[&name_hash, &version, &(block.height() as i64), &transaction.id().to_string(), &transition.id().to_string()]
        ).await.unwrap();

        println!(">> clear_resolver_record: {} {} {}", owner, name_hash, version)
    } else {
        println!(">> clear_resolver_record: Error")
    };
}

async fn burn(db_trans: &tokio_postgres::Transaction<'_>, block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    let outs = transition.outputs();
    let outs_last = outs.get(outs.len() - 1).unwrap();
    if let Some(may_future) = outs_last.future() {
        let args = may_future.arguments();
        let name_hash_arg = args.get(0).unwrap();

        let name_hash: String = parse_field(name_hash_arg).unwrap();

        db_trans.execute("DELETE from ans3.ans_name WHERE name_hash=$1", &[&name_hash]).await.unwrap();

        println!(">> burn: {}", name_hash)
    } else {
        println!(">> burn: Error")
    };
}

// parse argument
fn parse_str_4u128(name_arg: &Argument<N>) -> Result<String, String> {
    let name_bytes = Argument::to_bytes_le(name_arg).unwrap();
    let mut name: [u8; 64] = [0; 64];

    name[0..16].copy_from_slice(&name_bytes[11..27]);
    name[16..32].copy_from_slice(&name_bytes[32..48]);
    name[32..48].copy_from_slice(&name_bytes[53..69]);
    name[48..64].copy_from_slice(&name_bytes[74..90]);

    Ok(std::str::from_utf8(&name).unwrap().trim_matches('\0').to_string())
}

fn parse_str_8u128(name_arg: &Argument<N>) -> Result<String, String> {
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

fn parse_str_u128(name_arg: &Argument<N>) -> Result<String, String> {
    let name_bytes = Argument::to_bytes_le(name_arg).unwrap();
    let mut name: [u8; 16] = [0; 16];

    name[0..16].copy_from_slice(&name_bytes[4..20]);
    Ok(std::str::from_utf8(&name).unwrap().trim_matches('\0').to_string())
}

fn parse_field(field_arg: &Argument<N>) -> Result<String, String> {
    let field_arg_bytes = Argument::to_bytes_le(field_arg).unwrap();

    if field_arg_bytes.len() >= 32 {
        let last_32: &[u8] = &field_arg_bytes[field_arg_bytes.len() - 32..];
        Ok(format!("{}", Field::<N>::from_bytes_le(last_32).unwrap()))
    } else {
        Err("e".to_string())
    }
}

fn parse_address(address_arg: &Argument<N>) -> Result<String, String> {
    let address_arg_bytes = Argument::to_bytes_le(address_arg).unwrap();

    if address_arg_bytes.len() >= 32 {
        let last_32: &[u8] = &address_arg_bytes[address_arg_bytes.len() - 32..];
        Ok(format!("{}", Address::<N>::from_bytes_le(last_32).unwrap()))
    } else {
        Err("e".to_string())
    }
}