// Copyright (C) 2021 Subspace Labs, Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use std::path::PathBuf;
use substrate_wasm_builder::WasmBuilder;

fn main() {
    // Build Consensus WASM runtime.
    WasmBuilder::new()
        .with_current_project()
        .export_heap_base()
        .import_memory()
        .build();

    // Build Execution WASM runtime.
    let mut manifest_dir: PathBuf = std::env::var("CARGO_MANIFEST_DIR")
        .expect("`CARGO_MANIFEST_DIR` is always set for `build.rs` files; qed")
        .into();
    manifest_dir.pop();
    manifest_dir.pop();
    manifest_dir.push("cumulus/parachain-template/runtime/Cargo.toml");

    WasmBuilder::new()
        .with_project(&manifest_dir)
        .expect("Cirrus runtime directory must be valid")
        .export_heap_base()
        .import_memory()
        .set_file_name("execution_wasm_binary.rs")
        .build();
}
