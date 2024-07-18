# PicoEater

We cooked, pico ate.

This is a small CLI tool to:

- Dump a .p8 cart into a flurry of component files (lua scripts, and individual binary resources).
- Build a .p8 cart from a flurry of component files.

## Why

Made this for a game jam with some friends, where we had multiple people working concurrently on a pico8 cart via git. .p8 files are plain text, so that's good, but they have everything in one file, which didn't give us a good way to avoid concurrently touching "binary" resources (sfx, sprites) or make proper sense of script splits/merges.

Our solution was to just explode everything into separate files for version control, and build the compiled cart on demand. You've still gotta be careful with the binary data, but now you can at least track it by filename, and it's a lot easier to edit code in an external editor.

## Usage

- `picoeater dump thing.p8 --dir /some/directory`
- `picoeater build thing.p8 --dir /some/directory`

The `--dir` argument is optional and defaults to the current working directory.

The filename argument is also optional, IF the directory you're working with contains EXACTLY one existing .p8 file. Otherwise it's required.

### Script names, limits, etc.

Pico limits you to **sixteen script tabs.** Picoeater doesn't currently enforce that or protect you from it, so you're on your own to stay in line.

Picoeater maps lua script filenames to a first-line comment in the corresponding pico8 code editor tab. If there isn't one, it makes a fallback name you can change later, and then that'll be your first-line comment on next build.

We use the `_tab_order.p8meta` file to preserve your tab order across dump/build round-trips. You can edit it to change your tab order before a build, if you needed to split/merge some scripts.

### Extra files on dump

If you dump a cart and the directory happens to already have _extra component files_ that weren't present in the version of the cart you dumped, the tool will warn you, because it might mean something funky is happening. (It definitely means you're not getting the same cart back if you subsequently run a build.)

Use `dump --purge` to delete those extra files, after you check and decide you don't want 'em.

## Compiling

This is a Rust program, so you need to

- Use [rustup](https://rustup.rs/) to install a Rust compiler toolchain, if you've never done that before.
- cd to this directory.
- `cargo build --release`
- The resulting binary is at `./target/release/picoeater` (or `picoeater.exe` [if you're nasty](https://www.youtube.com/watch?v=ujnq2v6R02U)).
