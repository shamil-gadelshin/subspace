#!/usr/bin/env bash

set -u

request_local_peer_id() {
    curl --silent --location --request POST '127.0.0.1:9933' \
    --header 'Content-Type: application/json' \
    --data-raw '{
            "id":1,
            "jsonrpc":"2.0",
            "method":"system_localPeerId",
            "params":[]
    }'
}

PRIMARY_CHAIN_SPEC=subspace_dev.json

local_peer_id=$(request_local_peer_id | python3 -c "import sys, json; print(json.load(sys.stdin)['result'])")

EXECUTOR_BIN=./target/release/subspace-executor
BASE_PATH=second-db

echo
echo 'Starting Second Subspace Executor in Full node...'
rm -rf "$BASE_PATH" && "$EXECUTOR_BIN" \
    --alice \
    --force-authoring \
    --base-path "$BASE_PATH" \
    --pruning archive \
    --port 40233 \
    --log=sync=trace,parachain=trace,cirrus=trace,txpool=trace,gossip=trace,subspace::client=debug \
    --rpc-port 8745 \
    --ws-port 8746 \
    -- \
        --validator \
        --execution wasm \
        --log=trace \
        --chain "$PRIMARY_CHAIN_SPEC" \
        --port 30443 \
        --unsafe-rpc-external \
        --unsafe-ws-external \
        --bootnodes "/ip4/127.0.0.1/tcp/30333/p2p/$local_peer_id" \
        --ws-port 9987 \
        > second.log 2>&1
