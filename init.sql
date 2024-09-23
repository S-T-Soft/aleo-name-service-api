create schema ansb;

set search_path to ansb;

create table block
(
    id            serial,
    height        bigint,
    block_hash    text,
    previous_hash text,
    timestamp     bigint,
    created       bigint DEFAULT EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,

    constraint height_pk
        unique (height)
);

create table ans_nft_owner
(
    id         bigserial,
    name_hash  text not null,
    address    text not null,
    block_height   bigint,
    transaction_id text,
    transition_id text,
    constraint name_hash_pk
        unique (name_hash)
);

create index name_address_index
    on ans_nft_owner (address);

create table ans_name
(
    id         bigserial,
    name_hash  text          not null,
    name_field  text         not null,
    transfer_key text,
    parent     text,
    name       text          not null,
    full_name  text          not null,
    resolver   text,
    block_height   bigint,
    transaction_id text,
    transition_id text,
    constraint name_hash_pk2
        unique (name_hash)
);

create index ans_name_field_index
    on ans_name (name_field);

create table ans_primary_name
(
    id         bigserial,
    address    text not null,
    name_hash  text not null,
    block_height   bigint,
    transaction_id text,
    transition_id text,
    constraint primary_name_address_pk
        unique (address)
);

create table ans_resolver
(
    id         bigserial,
    name_hash  text      not null,
    category   text      not null,
    version    integer default 0 not null,
    name       text      not null,
    block_height   bigint,
    transaction_id text,
    transition_id text,
    constraint ans_resolver_pk
        unique (name_hash, category, version)
);

create table ans_name_version
(
    id         bigserial,
    name_hash  text      not null,
    version    integer default 0 not null,
    block_height   bigint,
    transaction_id text,
    transition_id text,
    constraint version_name_hash_pk
        unique (name_hash)
);

create table domain_credits
(
    id         bigserial,
    transfer_key  text      not null,
    amount    bigint default 0 not null,
    block_height   bigint,
    transaction_id text,
    transition_id text,
    constraint transfer_key_pk
        unique (transfer_key)
);

create table kv
(
    key    text,
    value  text,
    created       bigint DEFAULT EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    updated       bigint DEFAULT EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,

    constraint kv_key_pk
        unique (key)
);