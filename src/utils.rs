use std::time::{SystemTime, UNIX_EPOCH};
use std::cmp::min;
use std::str::FromStr;
use lazy_static::lazy_static;
use snarkvm_console_program::{Field, ToBits, Value};
use snarkvm_console_network::prelude::Zero;
use snarkvm_console_network::{Network, Testnet3, ToFields};
use tracing::info;

type N = Testnet3;

lazy_static! {
    static ref EMPTY_U128_ARR: Vec<Field<N>> = Value::<N>::try_from("[0u128, 0u128]").unwrap().to_fields().unwrap();
}

// pub fn string_to_u128(s: &str) -> Result<u128, String> {
//     // Check if all characters are valid
//     if !s.chars().all(|c| c.is_ascii_lowercase() || c.is_digit(10) || c == '-' || c == '_') {
//         return Err("Invalid character found".to_string());
//     }

//     let mut bytes = s.as_bytes().to_vec();

//     if bytes.len() > 16 {
//         return Err("The string is too long".to_string());
//     }

//     // Pad the vector with zeros
//     while bytes.len() < 16 {
//         bytes.push(0);
//     }

//     let mut bits = [0u8; 16];
//     bits.copy_from_slice(&bytes);

//     Ok( u128::from_le_bytes([bits[0], bits[1], bits[2], bits[3], bits[4], bits[5], bits[6], bits[7], bits[8], bits[9], bits[10], bits[11], bits[12], bits[13], bits[14], bits[15]]) )
// }

// Parse a name
pub fn parse_label_string(name: &str, valid: bool) -> Result<String, String> {
    // Check if all characters are valid
    if valid && !name.chars().all(|c| c.is_ascii_lowercase() || c.is_digit(10) || c == '-' || c == '_') {
        return Err("Invalid character found".to_string());
    }

    // Convert the string to a vector of u8
    let mut bytes = name.as_bytes().to_vec();

    // Check if the string is too long
    if bytes.len() > 64 {
        return Err("The name is too long".to_string());
    }

    // Pad the vector with zeros
    while bytes.len() < 64 {
        bytes.push(0);
    }

    // Convert the vector to a 512-bit integer
    let mut bits = [0u8; 64];
    bits.copy_from_slice(&bytes);

    // Split the integer into four parts
    let n1 = u128::from_le_bytes([bits[0], bits[1], bits[2], bits[3], bits[4], bits[5], bits[6], bits[7], bits[8], bits[9], bits[10], bits[11], bits[12], bits[13], bits[14], bits[15]]);
    let n2 = u128::from_le_bytes([bits[16], bits[17], bits[18], bits[19], bits[20], bits[21], bits[22], bits[23], bits[24], bits[25], bits[26], bits[27], bits[28], bits[29], bits[30], bits[31]]);
    let n3 = u128::from_le_bytes([bits[32], bits[33], bits[34], bits[35], bits[36], bits[37], bits[38], bits[39], bits[40], bits[41], bits[42], bits[43], bits[44], bits[45], bits[46], bits[47]]);
    let n4 = u128::from_le_bytes([bits[48], bits[49], bits[50], bits[51], bits[52], bits[53], bits[54], bits[55], bits[56], bits[57], bits[58], bits[59], bits[60], bits[61], bits[62], bits[63]]);

    Ok( format!("[{}u128, {}u128, {}u128, {}u128]", n1, n2, n3, n4) )
}

// Parse a label
pub fn parse_label(name: &str, parent: Field<N>) -> Result<Value<N>, String> {
    let name_str = parse_label_string(name, true)?;
    let names = format!("{{name: {}, parent: {}}}", &name_str, parent);

    info!("parse_label {} : {}", &name, &names);

    Ok (Value::<N>::try_from(&names).map_err(|e| e.to_string())?)
}

// Parse a name to hash
pub fn parse_name_hash(name: &str) -> Result<Field<N>, String> {
    // split name with dot，revert the order
    let mut name_parts: Vec<&str> = name.split('.').collect();
    name_parts.reverse();
    // convert the parts to hash
    let mut name_hash = Field::<N>::zero();
    for part in name_parts {
        let label = parse_label(part, name_hash)?;
        let avalue = label.to_fields().map_err(|e| e.to_string())?;
        name_hash = N::hash_psd2(&avalue).map_err(|e| e.to_string())?;
    }
    Ok(name_hash)
}


// Calc a name transfer key
pub fn get_name_transfer_key(name: &str) -> Result<Field<N>, String> {
    Ok(get_name_hash_transfer_key(&parse_name_hash(name)?.to_string())?)
}

pub fn get_name_hash_transfer_key(name_hash: &str) -> Result<Field<N>, String> {
    let salt = N::hash_to_scalar_psd2(&EMPTY_U128_ARR).unwrap();
    let name_hash = Value::<N>::from_str(&name_hash).unwrap();
    Ok(N::commit_bhp256(&name_hash.to_bits_le(), &salt).unwrap())
}


// pub fn reverse_parse_label(n1: u128, n2: u128, n3: u128, n4: u128) -> Result<String, String> {
//     let mut bytes = [0u8; 64];
//
//     bytes[0..16].copy_from_slice(&n1.to_le_bytes());
//     bytes[16..32].copy_from_slice(&n2.to_le_bytes());
//     bytes[32..48].copy_from_slice(&n3.to_le_bytes());
//     bytes[48..64].copy_from_slice(&n4.to_le_bytes());
//
//     let mut name = String::from_utf8(bytes.to_vec()).map_err(|_| "Failed to convert bytes to UTF-8")?;
//
//     name = name.trim_end_matches(char::from(0)).to_string();
//
//     Ok(name)
// }

pub fn split_string(input: &str) -> Vec<&str> {
    let mut result = Vec::new();

    for i in (0..input.len()).step_by(15) {
        result.push(&input[i..i + min(15, input.len() - i )]);
    }
    result
}

pub fn get_current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Failed to get current timestamp")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_name_transfer_key_valid() {
        let name = "888.ans";
        let expected = "177840532985979970251149184589146856693339066848489471550212639743537535986field";
        match get_name_transfer_key(&name) {
            Ok(value) => assert_eq!(value.to_string(), expected, "key not equal"),
            Err(e) => assert!(false, "Expected Ok, got Err {}", e),
        }
    }
}