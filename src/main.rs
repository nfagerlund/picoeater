use clap::{Parser, Subcommand};
use std::{
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

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
    #[command(subcommand)]
    commands: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Build a .p8 file from a collection of individual component files.
    Build {
        /// The directory the component files should go in. Defaults to the
        /// current working directory.
        #[arg(short, long)]
        dir: Option<PathBuf>,

        /// The combined .p8 file to operate on.
        file: PathBuf,
    },
    /// Dump a collection of individual component files from a .p8 file.
    Dump {
        /// The directory the component files should go in. Defaults to the
        /// current working directory.
        #[arg(short, long)]
        dir: Option<PathBuf>,

        /// The combined .p8 file to operate on.
        file: PathBuf,

        /// If there are component files in the target dir that aren't in
        /// the source .p8 file, delete them.
        #[arg(short, long)]
        purge: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.commands {
        Commands::Build { dir, file } => todo!(),
        Commands::Dump { dir, file, purge } => {
            // sort out the dir
            let cwd = std::env::current_dir()?;
            let abs_dir = cwd.join(dir.unwrap_or_else(PathBuf::new));

            let dumper = P8Dumper::new(file, abs_dir)?;
            let written = dumper.dump()?;
        }
    }

    Ok(())
}

struct P8Dumper {
    reader: BufReader<File>,
    dest: PathBuf,
}

enum ReadState {
    Init,
    // LuaStart gets its own thing because of those magic scissor lines.
    LuaStart,
    Lua { writer: BufWriter<File> },
    Rsc { writer: BufWriter<File> },
}

#[derive(thiserror::Error, Debug)]
enum DumpError {
    #[error("Somehow never got out of Init; either a bug or a corrupt .p8 file")]
    EndInInit,
    #[error("Somehow ended in LuaStart; either a bug or a corrupt .p8 file")]
    EndInLuaStart,
}

fn rsc_tag(line: &str) -> Option<&str> {
    if line.len() > 4 && &line[0..2] == "__" && &line[(line.len() - 2)..line.len()] == "__" {
        let rest = &line[2..(line.len() - 2)];
        // I'm gonna do a real fast and loose one here so I don't have to take a regexp
        // dep or hardcode the resource kinds.
        // basically I want it to be one "word" in there.
        if !rest.contains([' ', '=', '.']) {
            return Some(rest);
        }
    }
    None
}

impl P8Dumper {
    /// Make a new P8Reader from a provided absolute file path and dir path.
    pub fn new(path: impl AsRef<Path>, dest: PathBuf) -> std::io::Result<Self> {
        File::open(path).map(|file| Self {
            reader: BufReader::new(file),
            dest,
        })
    }

    /// Do the dump. Returns the list of files written.
    pub fn dump(self) -> anyhow::Result<Vec<String>> {
        // consume self
        let Self { reader, dest } = self;
        // initial state
        let mut state = ReadState::Init;
        // initial lua index
        let mut lua_index = 0u8;
        // Keep track of which files we wrote to, so we can purge if desired.
        let mut files_written: Vec<String> = Vec::new();

        // helper closure for resource writers, since we make those in two spots
        let mut make_writer = |filename: String| -> std::io::Result<BufWriter<File>> {
            let path = dest.join(&filename);
            // stow that filename
            files_written.push(filename);
            let file = File::create(path)?;
            Ok(BufWriter::new(file))
        };

        for item in reader.lines() {
            let line = item?;
            match &mut state {
                ReadState::Init => {
                    // Skip the header until you get to the lua section.
                    if line == "__lua__" {
                        state = ReadState::LuaStart;
                    }
                }
                ReadState::LuaStart => {
                    // Set up a new writer.
                    // Do we have a filename from an initial comment?
                    let mut filename = format!("{}", lua_index);
                    if &line[0..2] == "--" {
                        filename.push('.');
                        filename.push_str(&line[2..]);
                    }
                    filename.push_str(".lua");
                    let mut writer = make_writer(filename)?;
                    // Write that initial line so we don't drop it!
                    writer.write_all(line.as_ref())?;
                    writer.write_all("\n".as_ref())?;
                    // bump the index for next time
                    lua_index += 1;
                    // go.
                    state = ReadState::Lua { writer };
                }
                ReadState::Lua { writer } => {
                    if &line == "-->8" {
                        // we're done!!
                        writer.flush()?;
                        // NEXT,
                        state = ReadState::LuaStart;
                    } else if let Some(rsc_kind) = rsc_tag(&line) {
                        // we're done!
                        writer.flush()?;
                        // set up the resource writer.
                        let filename = format!("{}.p8rsc", &rsc_kind);
                        let writer = make_writer(filename)?;
                        // We don't write the tag line to the file.
                        state = ReadState::Rsc { writer };
                    } else {
                        // normal line. write!
                        writer.write_all(line.as_ref())?;
                        writer.write_all("\n".as_ref())?;
                    }
                }
                ReadState::Rsc { writer } => {
                    if let Some(rsc_kind) = rsc_tag(&line) {
                        // we're done. next!
                        writer.flush()?;
                        let filename = format!("{}.p8rsc", &rsc_kind);
                        let writer = make_writer(filename)?;
                        // We don't write the tag line to the file.
                        state = ReadState::Rsc { writer };
                    } else {
                        // normal line. write!
                        writer.write_all(line.as_ref())?;
                        writer.write_all("\n".as_ref())?;
                    }
                }
            }
        }
        // Do a final flush once we've consumed the whole file.
        match state {
            ReadState::Init => {
                return Err(DumpError::EndInInit.into());
            }
            ReadState::LuaStart => {
                return Err(DumpError::EndInLuaStart.into());
            }
            ReadState::Lua { mut writer } => {
                writer.flush()?;
            }
            ReadState::Rsc { mut writer } => {
                writer.flush()?;
            }
        }
        Ok(files_written)
    }
}
