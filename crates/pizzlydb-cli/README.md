# pizzlydb-cli

Admin commands, diagnostics, and the query shell.

This crate is a skeleton. The binary starts and exits. It does not open a database, parse a query, or call the server.

## Purpose

`pizzlydb-cli` will provide the `pizzlydb` executable for administration, diagnostics, and an interactive query shell. The package name is `pizzlydb-cli`. The binary name is `pizzlydb`.

The crate depends on `pizzlydb-sdk`.

From the workspace root:

```shell
cargo run -p pizzlydb-cli --quiet
```

The command exits 0 and writes no output.

## License

Licensed under the [Apache License 2.0](../../LICENSE).
