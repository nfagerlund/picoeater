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
const DEFAULT_RESOURCE_ORDER: [&str; 6] = ["gfx", "gff", "label", "map", "sfx", "music"];
const DEFAULT_P8_VERSION: &str = "41";

const RSC_ORDER_FILE: &str = "_rsc_order.p8meta";
const TAB_ORDER_FILE: &str = "_tab_order.p8meta";
const P8_VERSION_FILE: &str = "_version.p8meta";

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
            let DumpResults {
                tab_order,
                rsc_order,
            } = dumper.dump()?;
            let mut components = ComponentFiles::list(abs_dir)?;
            components.remove_script_names(&tab_order);
            components.remove_resource_kinds(&rsc_order);
            if !components.is_empty() {
                if purge {
                    println!("Purging extra component files not included in the source .p8:");
                    for path in components.iter() {
                        println!("  - {}", path.to_string_lossy());
                        std::fs::remove_file(path)?;
                    }
                } else {
                    println!("WARNING: The target directory contains extra component files that weren't included in the source .p8:\n");
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

struct DumpResults {
    tab_order: Vec<String>,
    rsc_order: Vec<String>,
}

impl P8Dumper {
    /// Make a new P8Reader from a provided absolute file path and dir path.
    pub fn new(path: impl AsRef<Path>, dest: PathBuf) -> std::io::Result<Self> {
        File::open(path).map(|file| Self {
            reader: BufReader::new(file),
            dest,
        })
    }

    /// Do the dump. Returns the list of lua scripts written, and the list of resources written.
    pub fn dump(self) -> anyhow::Result<DumpResults> {
        // consume self
        let Self { reader, dest } = self;
        // initial state
        let mut state = ReadState::Init;
        // initial lua index
        let mut lua_index = 0u8;
        // Keep track of which files we wrote to in which order. We use this for builds,
        // and for purging.
        let mut tab_order: Vec<String> = Vec::new();
        let mut rsc_order: Vec<String> = Vec::new();

        // helper closure for resource writers, since we make those in two spots
        let make_writer = |filename: &str| -> std::io::Result<BufWriter<File>> {
            let path = dest.join(filename);
            let file = File::create(path)?;
            Ok(BufWriter::new(file))
        };

        for item in reader.lines() {
            let line = item?;
            match &mut state {
                ReadState::Init => {
                    // Get version from the header, and wait for the lua section.
                    if line.starts_with("version") {
                        if let Some((_, ver)) = line.split_once(' ') {
                            std::fs::write(P8_VERSION_FILE, ver)?;
                        }
                    }

                    if line == "__lua__" {
                        state = ReadState::LuaStart;
                    }
                }
                ReadState::LuaStart => {
                    // Set up a new writer.
                    // Do we have a script name from an initial comment?
                    let maybe_name = lua_tag(&line);
                    let mut name = match maybe_name {
                        Some(tag) => tag.to_string(),
                        None => format!("unknown-{:02}", lua_index),
                    };
                    // If there's a name collision, do something gross to avoid calamity.
                    while tab_order.contains(&name) {
                        name.push_str("-again");
                    }
                    let filename = format!("{}.lua", &name);
                    let mut writer = make_writer(&filename)?;
                    // If we didn't get a name from the initial line, guess what:
                    // we'll damn well get one next time :] This makes THIS round-trip
                    // inexact, but it should help keep subsequent round-trips more stable.
                    if maybe_name.is_none() {
                        writer.write_all(format!("-- {}", &name).as_ref())?;
                        writer.write_all("\n".as_ref())?;
                    }
                    // Save the script name to tab order
                    tab_order.push(name);
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
                    let mut writer = make_writer(&filename)?;
                    // Write that initial line so we don't drop it!
                    writer.write_all(line.as_ref())?;
                    writer.write_all("\n".as_ref())?;
                    // Handoff to Rsc state
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
        // Write the tab order and resource order
        let mut tab_writer = make_writer(TAB_ORDER_FILE)?;
        for line in tab_order.iter() {
            tab_writer.write_all(line.as_ref())?;
            tab_writer.write_all("\n".as_ref())?;
        }
        tab_writer.flush()?;
        let mut rsc_writer = make_writer(RSC_ORDER_FILE)?;
        for line in rsc_order.iter() {
            rsc_writer.write_all(line.as_ref())?;
            rsc_writer.write_all("\n".as_ref())?;
        }
        rsc_writer.flush()?;
        Ok(DumpResults {
            tab_order,
            rsc_order,
        })
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
        let mut components = ComponentFiles::list(&source)?;
        // load the meta files
        let tab_order = read_optional_text_file(source.join(TAB_ORDER_FILE))?;
        let mut rsc_order = read_optional_text_file(source.join(RSC_ORDER_FILE))?;
        // tbh this shouldn't ever happen, but anyway:
        if rsc_order.trim().is_empty() {
            rsc_order = DEFAULT_RESOURCE_ORDER.join("\n");
            rsc_order.push('\n');
        }
        let mut version = read_optional_text_file(source.join(P8_VERSION_FILE))?;
        if version.trim().is_empty() {
            version = DEFAULT_P8_VERSION.to_string();
        }
        // write header
        writer.write_all(
            format!(
                "pico-8 cartridge // http://www.pico-8.com\nversion {}\n",
                version.trim()
            )
            .as_ref(),
        )?;
        // write luas
        writer.write_all("__lua__\n".as_ref())?;
        // ...btw, writing these requires some finesse, because 1. I can't
        // guarantee there's a newline at the end of each file, and 2. I
        // need to keep track of which file is last so we don't write an extra
        // scissors line.
        // Well, we'll just go line-by-line. less efficient, but safer.
        let mut first = true;
        // First write the known tab order
        for script_name in tab_order.lines() {
            if let Some(path) = components.lua.remove(script_name) {
                if !first {
                    // scissor line
                    writer.write_all("-->8\n".as_ref())?;
                }
                first = false;
                slurp_file_by_line(&mut writer, path)?;
            }
        }
        // Then leftover scripts in arbitrary order
        for path in components.lua.values() {
            if !first {
                // scissor line
                writer.write_all("-->8\n".as_ref())?;
            }
            first = false;
            slurp_file_by_line(&mut writer, path)?;
        }
        // Write known resources
        for kind in rsc_order.lines() {
            if let Some(path) = components.rsc.remove(kind) {
                writer.write_all(format!("__{}__\n", kind).as_ref())?;
                slurp_file_by_line(&mut writer, path)?;
            }
        }
        // Then leftover resources in arbitrary order
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
    lua: HashMap<String, PathBuf>,
    rsc: HashMap<String, PathBuf>,
}

fn osstr_eq_bytes(osstr: &OsStr, bytes: &[u8]) -> bool {
    osstr.as_encoded_bytes() == bytes
}

fn read_optional_text_file(path: impl AsRef<Path>) -> anyhow::Result<String> {
    match std::fs::read(path.as_ref()) {
        Ok(stuff) => Ok(String::from_utf8(stuff)?),
        Err(e) => match e.kind() {
            std::io::ErrorKind::NotFound => Ok("".to_string()),
            _ => Err(e.into()),
        },
    }
}

impl ComponentFiles {
    /// Takes an absolute directory path, finds and sorts the p8 stuff.
    fn list(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let mut lua_map = HashMap::new();
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
                // Skip filenames that don't have both stem and extension, they're deffo not ours.
                let (Some(stem), Some(ext)) = (path.file_stem(), path.extension()) else {
                    continue;
                };
                if osstr_eq_bytes(ext, b"lua") {
                    lua_map.insert(stem.to_string_lossy().into_owned(), path);
                } else if osstr_eq_bytes(ext, b"p8rsc") {
                    rsc.insert(stem.to_string_lossy().into_owned(), path);
                }
            }
        }
        Ok(Self { lua: lua_map, rsc })
    }

    /// Given a list of script names, remove any matching items from the collection.
    fn remove_script_names(&mut self, subset: &[impl AsRef<str>]) {
        for item in subset {
            self.lua.remove(item.as_ref());
        }
    }

    /// Given a list of resource kinds, remove any matching items from the collection.
    fn remove_resource_kinds(&mut self, subset: &[impl AsRef<str>]) {
        for item in subset {
            self.rsc.remove(item.as_ref());
        }
    }

    fn is_empty(&self) -> bool {
        self.lua.is_empty() && self.rsc.is_empty()
    }

    fn iter(&self) -> impl Iterator<Item = &PathBuf> {
        let lua = self.lua.values();
        let rsc = self.rsc.values();
        lua.chain(rsc)
    }
}

#[test]
fn hey() {
    let dir = PathBuf::from("/Users/nick/Documents/code/dr_chaos");
    let cf = ComponentFiles::list(dir).unwrap();
    println!("{:?}", cf);
}
