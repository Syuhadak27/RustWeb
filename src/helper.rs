//! CEF child-process helper (GPU, renderer, plugin, alerts, utility).

use cef::{args::Args, *};

fn main() {
    let args = Args::new();

    #[cfg(target_os = "macos")]
    let sandbox = sandbox_initialize(args.as_main_args().argc, args.as_main_args().argv);

    // `true` tells the loader that this is a helper process inside the .app bundle.
    #[cfg(target_os = "macos")]
    let _loader = {
        let loader = library_loader::LibraryLoader::new(&std::env::current_exe().unwrap(), true);
        assert!(loader.load(), "failed to load the Chromium Embedded Framework");
        loader
    };

    execute_process(
        Some(args.as_main_args()),
        None::<&mut App>,
        std::ptr::null_mut(),
    );

    #[cfg(target_os = "macos")]
    sandbox_destroy(sandbox.cast());
}