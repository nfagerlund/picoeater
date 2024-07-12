# PicoEater

We cooked, pico ate.

This is a small CLI tool to:

- Dump a .p8 cart into a flurry of component files (lua scripts, and individual binary resources).
- Build a .p8 cart from a flurry of component files.

## Usage

- `picoeater dump thing.p8 --dir /some/directory`
- `picoeater build thing.p8 --dir /some/directory`

The `--dir` argument is optional and defaults to the current working directory.

The filename argument is also optional, IF the directory you're working with contains EXACTLY one existing .p8 file. Otherwise it's required.

## Extra files on dump

If you dump a cart and the directory happens to already have _extra component files_ that weren't present in the version of the cart you dumped, the tool will warn you, because it might mean something funky is happening. (It definitely means you're not getting the same cart back if you subsequently run a build.)

You can run a second dump with the `--purge` option to delete those extra files, if you look into it and decide you don't want 'em.
