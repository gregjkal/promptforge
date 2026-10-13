use std::path::PathBuf;

use crate::update::windows::{Route, SETUP_ARGUMENTS, route, setup_file_name};

#[test]
fn windows_runs_the_setup_only_without_a_workshop_beside_the_gateway() {
    assert_eq!(route("windows", || None), Route::Setup);
    assert_eq!(
        route("windows", || Some(PathBuf::from(
            r"C:\PromptForge\promptforge-workshop.exe"
        ))),
        Route::WorkshopOwned
    );
}

#[test]
fn macos_and_linux_swap_without_looking_for_workshop() {
    for os in ["macos", "linux"] {
        assert_eq!(
            route(os, || unreachable!("the peer lookup runs only on Windows")),
            Route::Swap
        );
    }
}

#[test]
fn the_setup_runs_passive_in_update_mode_and_never_relaunches_workshop() {
    assert_eq!(SETUP_ARGUMENTS, ["/P", "/UPDATE"]);
    assert!(!SETUP_ARGUMENTS.contains(&"/R"));
}

#[test]
fn the_setup_keeps_its_published_name_when_it_is_a_plain_exe() {
    assert_eq!(
        setup_file_name("https://example.invalid/d/PromptForge_0.4.0_x64-setup.exe"),
        "PromptForge_0.4.0_x64-setup.exe"
    );
    assert_eq!(
        setup_file_name("https://example.invalid/d/PromptForge_0.4.0_x64-setup.exe?sig=1"),
        "PromptForge_0.4.0_x64-setup.exe"
    );
    for odd in [
        "https://example.invalid/d/setup.zip",
        "https://example.invalid/d/",
        "https://example.invalid/d/..%5Cevil.exe",
    ] {
        assert_eq!(setup_file_name(odd), "PromptForge-setup.exe", "{odd}");
    }
}
