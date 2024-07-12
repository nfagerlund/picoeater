use clap::{Parser, Subcommand};
use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
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

/// The order of known resources (other than lua!) in a .p8 file.
const RESOURCE_ORDER: [&str; 6] = ["gfx", "gff", "label", "map", "sfx", "music"];

/// Header for the version of p8 we happen to be using today.
const P8_HEADER: &str = "pico-8 cartridge // http://www.pico-8.com\nversion 41\n";

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
        /// The directory the component files should come from. Defaults to the
        /// current working directory.
        #[arg(short, long)]
        dir: Option<PathBuf>,

        /// The combined .p8 file to build. If there's only one existing .p8 in the
        /// source directory, it defaults to replacing that.
        file: Option<PathBuf>,
    },
    /// Dump a collection of individual component files from a .p8 file.
    Dump {
        /// The directory the component files should go in. Defaults to the
        /// current working directory.
        #[arg(short, long)]
        dir: Option<PathBuf>,

        /// The combined .p8 file to dump. If there's only one .p8 in the
        /// target directory, it defaults to that.
        file: Option<PathBuf>,

        /// If there are component files in the target dir that aren't in
        /// the source .p8 file, delete them.
        #[arg(short, long)]
        purge: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.commands {
        Commands::Build { dir, file } => {
            // sort out the dir
            let cwd = std::env::current_dir()?;
            let abs_dir = cwd.join(dir.unwrap_or_else(PathBuf::new));
            let real_file = match file {
                Some(f) => f,
                None => get_default_p8(&abs_dir)?,
            };

            let builder = P8Builder::new(real_file, abs_dir)?;
            builder.build()?;
        }
        Commands::Dump { dir, file, purge } => {
            // sort out the dir
            let cwd = std::env::current_dir()?;
            let abs_dir = cwd.join(dir.unwrap_or_else(PathBuf::new));
            let real_file = match file {
                Some(f) => f,
                None => get_default_p8(&abs_dir)?,
            };

            let dumper = P8Dumper::new(real_file, abs_dir.clone())?;
            let (written, tab_order, rsc_order) = dumper.dump()?;
            let mut components = ComponentFiles::list(abs_dir)?;
            components.difference(written);
            if !components.is_empty() {
                if purge {
                    println!("Purging extra component files not included in the source .p8:");
                    for path in components.iter() {
                        println!("  - {}", path.to_string_lossy());
                        std::fs::remove_file(path)?;
                    }
                } else {
                    println!("WARNING: The target directory contains the following extra component files, which weren't included in the source .p8:\n");
                    for path in components.iter() {
                        println!("  - {}", path.to_string_lossy());
                    }
                    println!("\nFor a quick way to delete these extra files, run dump again with the `--purge` flag.")
                }
            }
        }
    }

    Ok(())
}

#[derive(thiserror::Error, Debug)]
enum DefaultP8Error {
    #[error("No default .p8: zero existing .p8 files in the working directory.\nYou'll need to specify a filename.")]
    Zero,
    #[error("No default .p8: too many existing .p8 files in the working directory.\nYou'll need to specify a filename.")]
    TooMany,
}

fn get_default_p8(dir: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
    let p8ext = OsStr::new("p8");
    let mut p8s: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| match path.extension() {
            Some(ext) => ext == p8ext,
            None => false,
        })
        .collect();
    if p8s.is_empty() {
        return Err(DefaultP8Error::Zero.into());
    } else if p8s.len() > 1 {
        return Err(DefaultP8Error::TooMany.into());
    }
    Ok(p8s.pop().unwrap())
}

struct P8Dumper {
    reader: BufReader<File>,
    dest: PathBuf,
}

enum ReadState {
    Init,
    // LuaStart gets the script name on the next line, bc it goes "scissors \n comment".
    LuaStart,
    Lua { writer: BufWriter<File> },
    // RscStart needs to remember the kind, because the separator is one line, not two.
    RscStart { kind: String },
    Rsc { writer: BufWriter<File> },
}

#[derive(thiserror::Error, Debug)]
#[allow(clippy::enum_variant_names)]
enum DumpError {
    #[error("Somehow never got out of Init; either a bug or a corrupt .p8 file")]
    EndInInit,
    #[error("Somehow ended in LuaStart; either a bug or a corrupt .p8 file")]
    EndInLuaStart,
    #[error("Somehow ended in RscStart; either a bug or a corrupt .p8 file")]
    EndInRscStart,
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

// Note that this is only a valid question on the FIRST line of a lua
// file, so only the LuaStart state can use it.
fn lua_tag(line: &str) -> Option<&str> {
    if line.len() > 2 && &line[0..2] == "--" {
        let rest = line[2..].trim();
        if rest.is_empty() {
            None
        } else {
            Some(rest)
        }
    } else {
        None
    }
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
    pub fn dump(self) -> anyhow::Result<(Vec<String>, Vec<String>, Vec<String>)> {
        // consume self
        let Self { reader, dest } = self;
        // initial state
        let mut state = ReadState::Init;
        // initial lua index
        let mut lua_index = 0u8;
        // Keep track of which files we wrote to, so we can purge if desired.
        let mut files_written: Vec<String> = Vec::new();
        let mut tab_order: Vec<String> = Vec::new();
        let mut rsc_order: Vec<String> = Vec::new();

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
                    // TODO: save version
                    if line == "__lua__" {
                        state = ReadState::LuaStart;
                    }
                }
                ReadState::LuaStart => {
                    // Set up a new writer.
                    // Do we have a script name from an initial comment?
                    let name = match lua_tag(&line) {
                        Some(tag) => tag.to_string(),
                        None => format!("{:02}", lua_index),
                    };
                    let filename = format!("{}.lua", &name);
                    // Save the script name to tab order
                    tab_order.push(name);
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
                        // Next stop, resourceville
                        state = ReadState::RscStart {
                            kind: rsc_kind.to_string(),
                        };
                    } else {
                        // normal line. write!
                        writer.write_all(line.as_ref())?;
                        writer.write_all("\n".as_ref())?;
                    }
                }
                ReadState::RscStart { kind } => {
                    let kind = kind.clone();
                    let filename = format!("{}.p8rsc", &kind);
                    // also stash the kind to resource order
                    rsc_order.push(kind);
                    let writer = make_writer(filename)?;
                    state = ReadState::Rsc { writer };
                }
                ReadState::Rsc { writer } => {
                    if let Some(rsc_kind) = rsc_tag(&line) {
                        // we're done. next!
                        writer.flush()?;
                        state = ReadState::RscStart {
                            kind: rsc_kind.to_string(),
                        };
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
            ReadState::RscStart { .. } => {
                return Err(DumpError::EndInRscStart.into());
            }
            ReadState::Lua { mut writer } => {
                writer.flush()?;
            }
            ReadState::Rsc { mut writer } => {
                writer.flush()?;
            }
        }
        Ok((files_written, tab_order, rsc_order))
    }
}

#[derive(Debug)]
struct P8Builder {
    writer: BufWriter<File>,
    source: PathBuf,
}

/// Takes a mutable reference to a writer and a source filename, and
/// copies the source to the writer line-by-line, inserting "\n" newlines
/// after each line. This is way less efficient than std::io::copy(), but
/// it takes care of normalizing any missing final newlines, AND sorting
/// out any rogue CRLFs.
fn slurp_file_by_line<W, P>(writer: &mut W, path: P) -> std::io::Result<()>
where
    W: Write,
    P: AsRef<Path>,
{
    let reader = BufReader::new(File::open(path)?);
    for item in reader.lines() {
        let line = item?;
        writer.write_all(line.as_ref())?;
        writer.write_all("\n".as_ref())?;
    }
    Ok(())
}

impl P8Builder {
    /// Make a new builder struct, given absolute paths to a p8 file target
    /// and a source directory.
    pub fn new(path: impl AsRef<Path>, source: PathBuf) -> std::io::Result<Self> {
        File::create(path).map(|file| Self {
            writer: BufWriter::new(file),
            source,
        })
    }

    /// Do the build. Returns nothing on success.
    pub fn build(self) -> anyhow::Result<()> {
        let Self { mut writer, source } = self;
        // get the stuff
        let mut components = ComponentFiles::list(source)?;
        // write header
        writer.write_all(P8_HEADER.as_ref())?;
        // write luas
        writer.write_all("__lua__\n".as_ref())?;
        // ...btw, writing these requires some finesse, because 1. I can't
        // guarantee there's a newline at the end of each file, and 2. I
        // need to keep track of which file is last so we don't write an extra
        // scissors line.
        // Well, we'll just go line-by-line. less efficient, but safer.
        if !components.lua.is_empty() {
            // do the first one, then any extras with scissor lines.
            slurp_file_by_line(&mut writer, &components.lua[0])?;
            for path in &components.lua[1..] {
                // scissor line
                writer.write_all("-->8\n".as_ref())?;
                slurp_file_by_line(&mut writer, path)?;
            }
        }
        // write known resources
        for kind in RESOURCE_ORDER {
            if let Some(path) = components.rsc.remove(kind) {
                writer.write_all(format!("__{}__\n", kind).as_ref())?;
                slurp_file_by_line(&mut writer, path)?;
            }
        }
        // write any leftover resources in arbitrary order 🤷🏽
        for (kind, path) in components.rsc.iter() {
            writer.write_all(format!("__{}__\n", kind).as_ref())?;
            slurp_file_by_line(&mut writer, path)?;
        }
        // flush
        writer.flush()?;
        Ok(())
    }
}

#[derive(Debug)]
struct ComponentFiles {
    lua: Vec<PathBuf>,
    rsc: HashMap<String, PathBuf>,
}

fn osstr_eq_bytes(osstr: &OsStr, bytes: &[u8]) -> bool {
    osstr.as_encoded_bytes() == bytes
}

impl ComponentFiles {
    /// Takes an absolute directory path, finds and sorts the p8 stuff.
    fn list(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let mut lua = Vec::new();
        let mut rsc = HashMap::new();
        for item in std::fs::read_dir(dir.as_ref())? {
            // If it's a lua file, put it in the vec (then later sort the vec).
            // If it's a .p8rsc file, put it in the hashmap.
            // If it's anything else, ignore it.
            let entry = item?;
            if entry.file_type()?.is_file() {
                // doing an early allocating conversion to PathBuf so I can check
                // file extension without having to write my own .split() for OsStr -_-
                let path = entry.path();
                if let Some(ext) = path.extension() {
                    if osstr_eq_bytes(ext, b"lua") {
                        lua.push(path);
                    } else if osstr_eq_bytes(ext, b"p8rsc") {
                        if let Some(kind) = path.file_stem() {
                            rsc.insert(kind.to_string_lossy().into_owned(), path);
                        }
                    }
                }
            }
        }
        // Sort luas by filename; since they're numbered, this should put them back
        // in the order they arrived in. If you made conflicting numbers, the
        // resulting built order will be abritrary, and it'll sort itself out
        // on the next round trip.
        lua.sort_unstable();
        Ok(Self { lua, rsc })
    }

    /// Given a list of .lua and .p8rsc files, remove any items in that list
    /// from the collections.
    fn difference(&mut self, subset: Vec<String>) {
        let lua_ext = OsStr::new("lua");
        let rsc_ext = OsStr::new("p8rsc");
        for item in subset {
            let to_remove = Path::new(&item);
            let Some(ext) = to_remove.extension() else {
                continue;
            };
            if ext == lua_ext {
                self.lua
                    .retain(|file| file.file_name() != to_remove.file_name());
            } else if ext == rsc_ext {
                let Some(os_kind) = to_remove.file_stem() else {
                    continue;
                };
                let Some(kind) = os_kind.to_str() else {
                    continue;
                };
                self.rsc.remove(kind);
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.lua.is_empty() && self.rsc.is_empty()
    }

    fn iter(&self) -> impl Iterator<Item = &PathBuf> {
        let lua = self.lua.iter();
        let rsc = self.rsc.iter().map(|e| e.1);
        lua.chain(rsc)
    }
}

#[test]
fn hey() {
    let dir = PathBuf::from("/Users/nick/Documents/code/dr_chaos");
    let cf = ComponentFiles::list(dir).unwrap();
    println!("{:?}", cf);
}
