use clap::{Parser, Subcommand};
use std::path::PathBuf;

// Okay, so http://pico8wiki.com/index.php?title=P8FileFormat
// - I'm gonna handle multiple lua files, and preserve the order
//   they were found in the p8 file if applicable.
// - I'm not gonna handle multiple gfx etc. sections. Don't do that.
// - I'm gonna treat the sections non-exhaustively; not positive a new section
//   hasn't been added since that write-up. But anything after the defined
//   order goes randomly last.
//
// Here's the format:
// pico-8 cartridge // http://www.pico-8.com
// version 41
// __lua__
// -- dr chaos
// ...
// -->8
// -- splash screen
// ...
// __gfx__
// ...
//
// ...and eventually it ends.
//
// > The sections appear in the following order: a header, the Lua code (__lua__),
// > the spritesheet (__gfx__), the sprite flags (__gff__), the cartridge label
// > (__label__), the map (__map__), sound effects (__sfx__), and music patterns
// > (__music__). These sections are described in more detail below.

#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
    /// The directory the component files should go in. Defaults to the
    /// current working directory.
    #[arg(short, long)]
    dir: Option<PathBuf>,

    /// The combined .p8 file to operate on.
    file: PathBuf,

    #[command(subcommand)]
    commands: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Build,
    Dump,
}

fn main() {
    let cli = Cli::parse();
    println!("{:?}", &cli);
}
