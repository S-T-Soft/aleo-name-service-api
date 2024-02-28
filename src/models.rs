use serde::{Deserialize, Serialize};

// resp
#[derive(Serialize)]
pub struct AddressName {
    pub address: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct NameHash {
    pub name_hash: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct NameHashBalance {
    pub name_hash: String,
    pub name: String,
    pub balance: i64,
}

#[derive(Serialize)]
pub struct Resolver {
    pub name_hash: String,
    pub category: String,
    pub content: String,
    pub version: i32
}

#[derive(Serialize)]
pub struct ResolverContent {
    pub name_hash: String,
    pub category: String,
    pub version: i32,
    pub content: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct AnsToken {
    pub name: String,
    pub image: String,
    pub attributes: Vec<AnsTokenAttr>,
    #[serde(rename = "mintNumber")]
    pub mint_number: i32,
    #[serde(rename = "collectionLink")]
    pub collection_link: String,
    #[serde(rename = "collectionName")]
    pub collection_name: String,
    #[serde(rename = "collectionDescription")]
    pub collection_description: String,
    #[serde(rename = "sourceLink")]
    pub source_link: String,

}

#[derive(Serialize)]
pub struct AnsTokenAttr {
    pub trait_type: String,
    pub value: String,
}

#[derive(Serialize, Deserialize)]
pub struct AnsStatistic {
    pub healthy: bool,
    pub block_height: i64,
    pub cal_time: u64,
    pub total_names_24h: i64,
    pub total_names: i64,
    pub total_pri_names: i64,
    pub total_nft_owners: i64
}

// db
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
    pub balance: i64,
}

