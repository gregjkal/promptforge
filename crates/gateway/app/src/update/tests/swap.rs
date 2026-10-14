use std::io;
use std::path::Path;

use crate::update::UpdateError;
use crate::update::swap::{previous_path, restore, restore_with, swap, swap_with};

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let installed = dir.path().join("promptforge-gateway");
    let staged = dir.path().join("staged");
    std::fs::write(&installed, "old").expect("install");
    std::fs::write(&staged, "new").expect("stage");
    (dir, installed, staged)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read")
}

#[test]
fn a_swap_moves_the_install_aside_and_the_staged_item_in() {
    let (_dir, installed, staged) = fixture();
    std::fs::write(previous_path(&installed), "leftover").expect("an old previous");
    swap(&installed, &staged).expect("swap");
    assert_eq!(read(&installed), "new");
    assert_eq!(read(&previous_path(&installed)), "old");
    assert!(!staged.exists());
}

#[test]
fn a_failed_second_rename_puts_the_original_back() {
    let (_dir, installed, staged) = fixture();
    let mut calls = 0;
    let error = swap_with(&installed, &staged, |from, to| {
        calls += 1;
        if calls == 2 {
            Err(io::Error::other("injected"))
        } else {
            std::fs::rename(from, to)
        }
    })
    .expect_err("the second rename fails");
    assert!(
        matches!(&error, UpdateError::Swap { installed: named, .. } if *named == installed),
        "{error}"
    );
    assert_eq!(read(&installed), "old", "the original is back in place");
    assert!(!previous_path(&installed).exists());
    assert_eq!(read(&staged), "new");
}

#[test]
fn a_swap_that_cannot_put_the_original_back_names_where_it_waits() {
    let (_dir, installed, staged) = fixture();
    let mut calls = 0;
    let error = swap_with(&installed, &staged, |from, to| {
        calls += 1;
        if calls == 1 {
            std::fs::rename(from, to)
        } else {
            Err(io::Error::other("injected"))
        }
    })
    .expect_err("the second and third renames fail");
    let previous = previous_path(&installed);
    assert!(
        matches!(&error, UpdateError::Stranded { installed: named, previous: waiting, .. }
            if *named == installed && *waiting == previous),
        "{error}"
    );
    assert!(
        error.to_string().contains("could not be moved back"),
        "{error}"
    );
    assert_eq!(read(&previous), "old", "the previous gateway waits aside");
}

#[test]
fn a_failed_first_rename_changes_nothing() {
    let (_dir, installed, staged) = fixture();
    let error = swap_with(&installed, &staged, |_, _| {
        Err(io::Error::other("injected"))
    })
    .expect_err("the first rename fails");
    assert!(matches!(error, UpdateError::Swap { .. }));
    assert_eq!(read(&installed), "old");
    assert_eq!(read(&staged), "new");
}

#[test]
fn a_restore_puts_the_previous_item_back_and_keeps_the_rejected_one_aside() {
    let (dir, installed, staged) = fixture();
    swap(&installed, &staged).expect("swap");
    let rejected = dir.path().join("rejected");
    restore(&installed, &rejected).expect("restore");
    assert_eq!(read(&installed), "old");
    assert_eq!(read(&rejected), "new");
    assert!(!previous_path(&installed).exists());
}

#[test]
fn a_failed_restore_leaves_the_new_item_installed() {
    let (dir, installed, staged) = fixture();
    swap(&installed, &staged).expect("swap");
    let rejected = dir.path().join("rejected");
    let mut calls = 0;
    restore_with(&installed, &rejected, |from, to| {
        calls += 1;
        if calls == 2 {
            Err(io::Error::other("injected"))
        } else {
            std::fs::rename(from, to)
        }
    })
    .expect_err("the previous item cannot come back");
    assert_eq!(read(&installed), "new", "something stays installed");
    assert_eq!(read(&previous_path(&installed)), "old");
}
