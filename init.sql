create schema ans3;

create table ans3.block
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

create table ans3.ans_nft_owner
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
    on ans3.ans_nft_owner (address);

create table ans3.ans_name
(
    id         bigserial,
    name_hash  text          not null,
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


create table ans3.ans_primary_name
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

create table ans3.ans_resolver
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

create table ans3.ans_name_version
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