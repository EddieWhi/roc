use crate::can::symbol::Symbol;
use crate::collections::{SendMap, SendSet};
use crate::ident::UnqualifiedIdent;
use crate::module::ModuleName;
use crate::parse::ast::{Attempting, Def, ExposesEntry, ImportsEntry, Module};
use crate::parse::module;
use crate::parse::parser::{Fail, Parser, State};
use crate::region::{Located, Region};
use im::Vector;
use bumpalo::Bump;
use tokio::fs::read_to_string;
use std::io;
use std::path::{Path, PathBuf};
use tokio::prelude::*;


pub struct Loaded<'a> {
    pub requested_header: LoadedHeader,
    pub dependent_headers: SendMap<ModuleName<'a>, LoadedHeader>,
    pub defs: SendMap<ModuleName<'a>, Result<Vector<Located<Def<'a>>>, Fail>>,
}

struct Env {
    pub src_dir: PathBuf
}

#[derive(Debug)]
pub enum BuildProblem<'a> {
    FileNotFound(&'a Path),
}

#[derive(Debug, PartialEq, Eq)]
pub enum LoadedHeader {
    Valid {
        declared_name: Option<Box<str>>,
        deps: SendSet<Box<str>>,
        scope: SendMap<Box<str>, (Symbol, Region)>,
        bytes_parsed: usize
    },
    FileProblem(io::ErrorKind),
    ParsingFailed(Fail),
}

pub async fn load<'a>(src_dir: PathBuf, filename: PathBuf) -> Loaded<'a> {
    let handle = tokio::spawn(async move {
        let mut env = Env {
            src_dir
        };

        load_filename(&mut env, &filename).await
    });

    let requested_header = handle.await;

    panic!("TODO");


//     // TODO parse defs on a different thread, and parse them
//     // directly into a Vector rather than into a Vec, so
//     // we don't have to reallocate here.
//     let defs = match module::module_defs().parse(&arena, state) {
//         Ok((defs, _)) => {
//             let mut send_vec = Vector::new();

//             for def in defs {
//                 send_vec.push_back(def);
//             }

//             Ok(send_vec)
//         },
//         Err((fail, _)) => Err(fail),
//     };

    // Loaded {
    //     requested_header,
    //     dependent_headers: env.loaded_headers,
    //     defs,
    //     problems: env.problems,
    // }
}

/// The long-term plan is for the loading process to work like this, starting from main.roc:
///
/// 1. Open the file.
/// 2. Parse its header.
/// 3. For each of its imports, repeat this process starting with step 1.
/// 4. Once a given import is finished parsing, we can process that import.
/// 5. Processing an import entails checking what we want to import against what it exposes.
/// 6. If anything we want to import unqualified is not exposed, record a problem.
/// 7. Add everything we were able to import unqualified to the module's default scope.
/// 8. Once all imports have been processed for this module, canonicalize it.
///
/// This would ideally be done using a parallel work-stealing scheduler like tokio_threadpool.
/// However, a prerequisite of this is that we are able to canonicalize in parallel!
///
/// To canonicalize in parallel, we want to be able to generate Variables in parallel,
/// which currently would require a Mutex on Subs. We can avoid that Mutex in one of two ways.
///
/// One way would be to give each thread in a thread pool a "starting id" -
/// distributed into (usize::MAX / n) ranges.  For example, if there are 2 threads,
/// the first thread gets to start at id 0, and the second thread starts at
/// id (usize::MAX / 2). That way both of them can increment in parallel without colliding.
/// (If we have 1024 threads running at once, on a 64-bit system, we still have
/// over 1 quadrillion Variables per thread. Seems like enough.)
/// However, to support that, we need to change Subs to be able to look up arbitrary IDs,
/// instead of being backed by a flat Vec where each Variable is a direct array index.
///
/// A strategy I like better, which should be slightly slower for canonicalization
/// (which is likely I/O bound anyway since it'll be happening concurrently with file reads),
/// but *much* faster for unification, is to give each thread a shared AtomicUsize which
/// they each call .fetch_add(1) on to get a fresh ID. Atomic increment is a bit slower than
/// regular increment, but it means afterwards unification (which I'm not yet sure how to
/// parallelize) no longer needs to use a hashing function to get the contents of each ID;
/// the IDs will already correspond directly to array indices like they do in the status quo.
///
/// Separately, if we use that strategy, there's probably another optimization opportunity:
/// instead of instantiating fresh structs with mk_fresh_var(), ensure that the default of
/// each struct will be all 0s in memory. That way, after we've distributed all the IDs,
/// we can do one single Vec resize (to zeroed memory) and they're all instantly ready to go.
///
/// Anyway, that'll all take awhile; for now, we'll do this in a synchronous, blocking way.

/// Resolve a module's list of imports, creating a Scope map for use in the
/// module's canonicalization.
///
/// If a given import has not been loaded yet, load it too.
// fn load_module<'a, 'p>(env: &mut Env<'a, 'p>, module_name: &ModuleName<'a>) -> LoadedHeader<'a> {
//     // 1. Convert module_name to filename, using src_dir.
//     // 2. Open that file for reading. (If there's a problem, record it and bail.)
//     // 3. Read the whole file into a string. (In the future, we can read just the header.)
//     // 4. Parse the header.
//     // 5. Use the parsed header to load more modules as necessary.
//     // 6. Now that all the headers have been parsed, parse the bodies too.
//     // 7. Once all the bodies have been parsed, canonicalize beginning with the leaves.

//     let mut filename = PathBuf::new();

//     filename.push(env.src_dir);

//     // Convert dots in module name to directories
//     for part in module_name.as_str().split('.') {
//         filename.push(part);
//     }

//     // End with .roc
//     filename.set_extension("roc");

//     load_filename(env, &filename)
// }

async fn load_filename(env: &mut Env, filename: &Path) -> LoadedHeader {
    match read_to_string(filename).await {
        Ok(src) => {
            let arena = Bump::new();
            // TODO instead of env.arena.alloc(src), we should create a new buffer
            // in the arena as a Vec<'a, u8> and call .as_mut_slice() on it to
            // get a (&mut [u8]) which can be passed to io::Read::read directly
            // instead of using read_to_string. This way, we avoid both heap-allocating
            // the String (which read_to_string does) and also re-allocating it
            // in the arena after read_to_string completes.
            let state = State::new(&src, Attempting::Module);

            let answer = match module::module().parse(&arena, state) {
                Ok((Module::Interface { header }, state)) => {
                    let declared_name = Some(header.name.value.as_str().into());

                    let mut scope = SendMap::default();
                    let mut deps = SendSet::default();

                    for loc_entry in header.imports {
                        deps.insert(load_import(env, loc_entry.region, &loc_entry.value, &mut scope));
                    }

                    let bytes_parsed = state.bytes_consumed();

                    LoadedHeader::Valid { scope, declared_name, deps, bytes_parsed }
                }
                Ok((Module::App { header }, state)) => {
                    // The app module has no declared name.
                    let declared_name = None;

                    let mut scope = SendMap::default();
                    let mut deps = SendSet::default();

                    for loc_entry in header.imports {
                        deps.insert(load_import(env, loc_entry.region, &loc_entry.value, &mut scope));
                    }

                    let bytes_parsed = state.bytes_consumed();

                    LoadedHeader::Valid { scope, declared_name, deps, bytes_parsed }
                }
                Err((fail, _)) => LoadedHeader::ParsingFailed(fail),
            };

            answer
        }
        Err(err) => LoadedHeader::FileProblem(err.kind()),
    }
}

fn load_import<'a, 'p, 'out>(
    env: &mut Env,
    region: Region,
    entry: &ImportsEntry<'_>,
    scope: &mut SendMap<Box<str>, (Symbol, Region)>,
) -> Box<str> {
    use crate::parse::ast::ImportsEntry::*;

    match entry {
        Module(module_name, exposes) => {
            for loc_entry in exposes {
                let (key, value) = expose(*module_name, &loc_entry.value, loc_entry.region);

                scope.insert(key, value);
            }

            module_name.as_str().into()
        }

        SpaceBefore(sub_entry, _) | SpaceAfter(sub_entry, _) => {
            // Ignore spaces.
            load_import(env, region, *sub_entry, scope)
        }
    }
}

fn expose<'out>(
    module_name: ModuleName<'_>,
    entry: &ExposesEntry<'_>,
    region: Region,
)->  (Box<str>, (Symbol, Region)){
    use crate::parse::ast::ExposesEntry::*;

    match entry {
        Ident(ident) => {
            // Since this value is exposed, add it to our module's default scope.
            let symbol = Symbol::from_module(&module_name, &ident);

            (ident.as_str().into(), (symbol, region))
        }
        SpaceBefore(sub_entry, _) | SpaceAfter(sub_entry, _) => {
            // Ignore spaces.
            expose(module_name, *sub_entry, region)
        }
    }
}

#[test]
fn test_tokio() {
    test_async(async {
        let handle = tokio::spawn(async {
            println!("doing some work, asynchronously");

            // Return a value for the example
            "result of the computation"
        });

        // Wait for the spawned task to finish
        let res = handle.await;

        println!("got {:?}", res);
    })
}

fn test_async<F: std::future::Future>(future: F) -> F::Output {
    use tokio::runtime::Runtime;

    // Create the runtime
    let mut rt = Runtime::new().expect("Error initializing Tokio runtime.");

    // Spawn the root task
    rt.block_on(future)
}
