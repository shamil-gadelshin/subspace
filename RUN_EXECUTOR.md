# Executor local devnet

```bash
cargo build --release
```

Compile everything before running the commands below.

1. Run a farmer:

```bash
$ rm -rf s && ./target/release/subspace-node --dev -d s --log=sync=trace,parachain=trace,txpool=trace,gossip::executor=trace > s.log 2>&1
```

```bash
$ ./target/release/subspace-farmer wipe && ./target/release/subspace-farmer farm
```

2. Run an Executor authority node:

```bash
$ ./start_first_executor.sh
```

3. Run an Executor full node:

```bash
$ ./start_second_executor.sh
```
