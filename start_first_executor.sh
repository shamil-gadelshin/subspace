#!/usr/bin/env bash

set -u

request_node_local_peer_id() {
    curl --silent --location --request POST '127.0.0.1:9933' \
    --header 'Content-Type: application/json' \
    --data-raw '{
            "id":1,
            "jsonrpc":"2.0",
            "method":"system_localPeerId",
            "params":[]
    }'
}

SUBSPACE_NODE=./target/release/subspace-node
PRIMARY_CHAIN_SPEC=subspace_dev.json

echo 'Building raw chain spec...'
$SUBSPACE_NODE build-spec --chain=dev --raw --disable-default-bootnode > "$PRIMARY_CHAIN_SPEC"

node_local_peer_id=$(request_node_local_peer_id | python3 -c "import sys, json; print(json.load(sys.stdin)['result'])")

EXECUTOR_BIN=./target/release/subspace-executor
BASE_PATH=executor-db

echo
echo 'Starting Subspace Executor in Authority node...'
rm -rf "$BASE_PATH" && "$EXECUTOR_BIN" \
    --alice \
    --collator \
    --force-authoring \
    --base-path "$BASE_PATH" \
    --pruning archive \
    --port 40333 \
    --log=sync=trace,parachain=trace,cirrus=trace,txpool=trace,gossip=trace \
    --rpc-port 8845 \
    --ws-port 8846 \
    -- \
        --validator \
        --execution wasm \
        --log=trace \
        --chain "$PRIMARY_CHAIN_SPEC" \
        --port 30343 \
        --unsafe-rpc-external \
        --unsafe-ws-external \
        --bootnodes "/ip4/127.0.0.1/tcp/30333/p2p/$node_local_peer_id" \
        --ws-port 9977 \
        > first.log 2>&1
