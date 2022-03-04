# Decoupled Execution

The following content is written against [this commit](https://github.com/subspace/subspace/tree/8ff41ea5b66a2345dfbbbea11973c94bc448097c) and may change in the future.

- `cumulus`
    - `client`
        - `block-builder`: custom block builder based on the original substrate block builder for executing the block with collected bundles.
        - `cirrus-executor`: core of the executor node.
        - `cirrus-service`
        - `cli`
        - `consensus`
        - `executor-gossip`: gossip the messages between the executor nodes.
    - `pallets`
        - `executive`: custom custive based on the original substrate pallet-executive for collecting the intermediate storage roots.
    - `parachain-template`
        - `node`: the entrypoint for secondary node(executor) binary.
        - `runtime`
    - `primitives`

- `polkadot`: mainly for the overseer, which is responsible for sending messages from the executor to the farmer network.
    - `collation-generation`: main bridge between the executor node and farmer node.
