# Aleo Name Service API Documentation

This project exposes several API endpoints for the purposes of name-to-hash conversions, obtaining the primary name of an address, getting the address of a name, and resolving a category and name to content. 

## API Endpoints

### 1. `GET /name_to_hash/{name}`

This API endpoint accepts a path parameter `name` and returns a `NameHash` object.

- **URL Params:** `name` (required)
- **Success Response:** `200 OK` with JSON body: `{ "name_hash": "<name_hash>", "name": "<name>" }`
- **Error Response:** `500 Internal Server Error` with JSON body: `{ "error": "Error parsing name: <error>" }`

### 2. `GET /primary_name/{address}`

This API endpoint accepts a path parameter `address` and returns an `AddressName` object.

- **URL Params:** `address` (required)
- **Success Response:** `200 OK` with JSON body: `{ "address": "<address>", "name": "<name>" }`
- **Error Response:** `404 Not Found`

### 3. `GET /address/{name}`

This API endpoint accepts a path parameter `name` and returns an `AddressName` object.

- **URL Params:** `name` (required)
- **Success Response:** `200 OK` with JSON body: `{ "address": "<address>", "name": "<name>" }`
- **Error Response:** `404 Not Found`

### 4. `GET /resolver`

This API endpoint accepts query parameters `name` and `category` and returns a `ResolverContent` object.

- **Query Params:** `name` and `category` (both required)
- **Success Response:** `200 OK` with JSON body: `{ "content": "<content>", "name": "<name>", "category": "<category>" }`
- **Error Response:** `404 Not Found`

### 5. `GET /public_ans/{address}`

This API endpoint accepts a path parameter `address` and returns `Vec<NFTWithPrimary>` object.

- **URL Params:** `address` (required)
- **Success Response:** `200 OK` with JSON body: `{ "address": "<address>", "name": "<name>", "name_hash": "<name_hash>", "is_primary_name": "<is_primary_name>", "resolver": "<resolver>" }`
- **Error Response:** `404 Not Found`

### 6. `GET /subdomain/{name}`
This API endpoint accepts a path parameter `name` and returns List subdomains

### 7. `GET /resolver/{name}`
This API endpoint accepts a path parameter `name` and returns List ResolverContent

### 8. `GET /token/{name_hash}`
This API endpoint accepts a path parameter `name_hash` and returns token

### 9. `GET /token/{name_hash}.svg`
This API endpoint accepts a path parameter `name_hash` and returns an avatar for a name hash 

### 10. `GET /statistic`
This API endpoint return the server statuses
```json
{
  "cal_time": 1701796297,
  "block_height": 1264452,
  "total_names_24h": 163,
  "total_names": 2226,
  "total_pri_names": 1298,
  "total_nft_owners": 888
}
```

## Running the server

### Local host
Start PostgresQL with init.sql and Redis before running the program.
To start the server in local, run the following command:

```bash
export REDIS_URL=redis://locahost:6379/0
export DATABASE_URL=postgresql://user:pwd@locahost:5432/ans
cargo run
```

### With docker-compose
```bash
docker-compose up
```

The server will start on localhost port 8080.

## Note
Error handling is implemented in the code, so if an error occurs (like parsing errors), the server will respond with an appropriate HTTP status code and error message.

Please feel free to contribute to this project or open issues if you encounter any problems.