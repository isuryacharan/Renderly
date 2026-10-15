fn main() {
    tauri_build::build();
    // tauri-build embeds the Common Controls v6 manifest only in the app binaries. Test binaries
    // would otherwise load comctl32 v5, which lacks TaskDialogIndirect, and fail at start-up with
    // STATUS_ENTRYPOINT_NOT_FOUND.
    // `rustc-link-arg-tests` would miss the library's own unit-test binary, so this applies to
    // every link step of the package.
    #[cfg(windows)]
    {
        println!(
            "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
}
