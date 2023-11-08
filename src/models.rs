use serde::Serialize;

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
}

#[derive(Serialize)]
pub struct Resolver {
    pub name_hash: String,
    pub category: String,
    pub name: String,
    pub version: i32
}