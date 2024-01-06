// indexer.rs

use std::error::Error;
use std::time::Duration;
use std::sync::{Arc, Mutex};
use futures::stream::{self, StreamExt};
use lazy_static::lazy_static;
use tokio::time::sleep;
use snarkvm_console_network::{FromBits, Testnet3, ToBits};
use snarkvm_ledger_block::{Block, Transaction};
use reqwest;
use snarkvm_console_program::{Field, Identifier, FromField};
use snarkvm_ledger_block::{Transition};
use tokio::io::{AsyncWriteExt, stdout};

type N = Testnet3;
static MAX_BLOCK_RANGE: u32 = 50;
const CDN_ENDPOINT: &str = "https://s3.us-west-1.amazonaws.com/testnet3.blocks/phase3";

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

pub async fn sync_data() {
    let _ = sync_from_cdn().await;
    loop {
        let block_number = match get_next_block_number().await {
            Ok(number) => number,
            Err(e) => {
                eprintln!("Error fetching next block number: {}", e);
                -1
            }
        };

        if block_number > -1 {
            let url = format!("https://api.explorer.aleo.org/v1/testnet3/block/{}", block_number);

            match reqwest::get(&url).await {
                Ok(response) => {
                    if let Ok(data) = response.json::<Block<N>>().await {
                        index_data(&data).await;
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


async fn sync_from_cdn() -> Result<(), Box<dyn Error>> {
    // local block height
    let start = 0u32;
    let end = get_latest_height().await?;
    let total_blocks = end.saturating_sub(start);

    println!("Sync {total_blocks} blocks from CDN (0% complete)...");

    let cdn_request_start = start.saturating_sub(start % MAX_BLOCK_RANGE);
    let cdn_request_end = end.saturating_sub(end % MAX_BLOCK_RANGE);

    let mut blocks_to_process = Arc::new(Mutex::new(Vec::new()));
    let blocks_to_process_clone = blocks_to_process.clone();

    // Scan the blocks via the CDN.
    let _ = snarkos_node_cdn::load_blocks(
        &CDN_ENDPOINT,
        cdn_request_start,
        Some(cdn_request_end),
        move |block| {
            // Check if the block is within the requested range.
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

        index_data(&block).await;
    }

    Ok(())
}

async fn get_latest_height() -> Result<u32, Box<dyn Error>> {
    let url = "https://api.explorer.aleo.org/v1/testnet3/block/height/latest";

    let response = reqwest::get(url).await?;

    let body = response.text().await?;

    Ok(body.parse::<u32>()?)
}

async fn get_next_block_number() -> Result<i64, Box<dyn Error>> {
    let latest_height = get_latest_height().await?;
    println!("Latest height: {}", latest_height);
    let local_latest_height = 1096997;
    let height = if latest_height > local_latest_height {
        local_latest_height as i64 + 1
    } else {
        -1
    };
    Ok(height)
}

async fn index_data(block: &Block<N>) {
    println!("Process block {} on {}", block.height(), block.timestamp());
    for transaction in block.transactions().clone().into_iter() {
        if transaction.is_accepted() {
            for transition in transaction.transitions() {
                if transition.program_id().name() == &*PROGRAM_ID {
                    println!("> process transition {}, function name: {}",
                             transition.id(), transition.function_name());
                    match transition.function_name() {
                        name if name == &*REGISTER => register(&block, &transaction, transition),
                        name if name == &*REGISTER_TLD => register(&block, &transaction, transition),
                        name if name == &*REGISTER_PRIVATE => register(&block, &transaction, transition),
                        name if name == &*REGISTER_PUBLIC => register(&block, &transaction, transition),
                        name if name == &*CONVERT_PRIVATE_TO_PUBLIC => convert_private_to_public(&block, &transaction, transition),
                        name if name == &*CONVERT_PUBLIC_TO_PRIVATE => convert_public_to_private(&block, &transaction, transition),
                        name if name == &*TRANSFER_PUBLIC => transfer_public(&block, &transaction, transition),
                        name if name == &*SET_PRIMARY_NAME => set_primary_name(&block, &transaction, transition),
                        name if name == &*UNSET_PRIMARY_NAME => unset_primary_name(&block, &transaction, transition),
                        name if name == &*SET_RESOLVER => set_resolver(&block, &transaction, transition),
                        name if name == &*SET_RESOLVER_RECORD => set_resolver_record(&block, &transaction, transition),
                        name if name == &*UNSET_RESOLVER_RECORD => unset_resolver_record(&block, &transaction, transition),
                        name if name == &*CLEAR_RESOLVER_RECORD => clear_resolver_record(&block, &transaction, transition),
                        name if name == &*BURN => burn(&block, &transaction, transition),
                        _ => {}
                    }
                }
            }
        }
    }
}

/**
process all register transition
**/
fn register(block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    for output in transition.outputs() {
        println!(">> output: {}", output)
    }
}

fn convert_private_to_public(block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    for output in transition.outputs() {
        println!(">> output: {}", output)
    }
}

fn convert_public_to_private(block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    for output in transition.outputs() {
        println!(">> output: {}", output)
    }
}

fn transfer_public(block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    for output in transition.outputs() {
        println!(">> output: {}", output)
    }
}

fn set_primary_name(block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    for output in transition.outputs() {
        println!(">> output: {}", output)
    }
}

fn unset_primary_name(block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    for output in transition.outputs() {
        println!(">> output: {}", output)
    }
}

fn set_resolver(block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    for output in transition.outputs() {
        println!(">> output: {}", output)
    }
}

fn set_resolver_record(block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    for output in transition.outputs() {
        println!(">> output: {}", output)
    }
}

fn unset_resolver_record(block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    for output in transition.outputs() {
        println!(">> output: {}", output)
    }
}

fn clear_resolver_record(block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    for output in transition.outputs() {
        println!(">> output: {}", output)
    }
}

fn burn(block: &Block<N>, transaction: &Transaction<N>, transition: &Transition<N>) {
    for output in transition.outputs() {
        println!(">> output: {}", output)
    }
}