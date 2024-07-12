#!/bin/bash

cargo build --release --target x86_64-pc-windows-gnu --target x86_64-apple-darwin --target aarch64-apple-darwin
zip rename_me.zip ./target/x86_64-pc-windows-gnu/release/picoeater.exe ./target/x86_64-apple-darwin/release/picoeater ./target/aarch64-apple-darwin/release/picoeater
