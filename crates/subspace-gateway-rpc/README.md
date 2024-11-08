## Subspace Gateway RPCs

RPC API for Subspace Gateway.

### Using the gateway RPCs

The gateway RPCs can fetch data using object mappings supplied by a node.

Launch a node with `--create-object-mappings blockNumber --sync full`, and wait for mappings from
the node RPCs. (See the node README for more details.)

The `blockNumber` should be taken from the last full block mappings received by the client.
Blocks with lots of mappings can be split into multiple batches, so the client can only be sure it
has received all the mappings when it sees the next block number.

```sh
$ subspace-node --create-object-mappings *blockNumber* --sync full ...
$ websocat --jsonrpc ws://127.0.0.1:9944
subspace_subscribeObjectMappings
```

```json
{
  "jsonrpc": "2.0",
  "method": "subspace_object_mappings",
  "params": {
    "subscription": "o7M85uu9ir39R5PJ",
    "result": {
      "blockNumber": 0,
      "v0": {
        "objects": [
          [
            "0000000000000000000000000000000000000000000000000000000000000000",
            0,
            0
          ]
        ]
      }
    }
  }
}
```

Then use those mappings to get object data from the gateway RPCs:
```sh
$ websocat --jsonrpc ws://127.0.0.1:9955
subspace_fetchObject {"mappings": {"v0": {"objects": [["0000000000000000000000000000000000000000000000000000000000000000", 0, 0]]}}}
```

```json
{
  "jsonrpc": "2.0",
  "result": ["00000000"]
}
```

#### Missed Mappings

The node doesn't make sure the client has processed the previous mapping before generating the next
one. And any mappings generated while the client is disconnected are silently dropped.

So mappings can be missed if the client is slow to connect, disconnects, or lags.
To avoid dropping mappings, do the equivalent of:
```sh
$ websocat -t - autoreconnect:jsonrpc:ws://127.0.0.1:9944
$ subspace-node --create-object-mappings blockNumber --sync full ...
```

This makes sure the websocket will connect as soon as the node opens its RPC port.
For example, the [`reconnecting-websocket` library](https://github.com/joewalnes/reconnecting-websocket).

#### Live Mappings Only

If the client is only interested in live updates, and can tolerate missing some mappings, the node
can use snap sync, and launch with `--create-object-mappings continue`:
```sh
$ subspace-node --create-object-mappings continue --sync snap ...
$ websocat --jsonrpc ws://127.0.0.1:9944
subspace_subscribeObjectMappings
```
