//! Unit tests for failure, interruption, and sidecar cleanup paths of both
//! modes.

use super::installer_tests::{
    VERSION, bundle, bundled, collect_signature, gateway, node_found, output, request,
    version_printed,
};
use super::*;

fn debug_request(target: &str) -> BuildRequest {
    BuildRequest {
        profile: Profile::Debug,
        target: Some(target.to_owned()),
    }
}

fn debug_gateway(environment: &BuildEnvironment, target: &str) -> PathBuf {
    environment
        .target_root
        .join(target)
        .join("debug")
        .join(sidecar::gateway_binary_name(target))
}

/// Stands in for a file the cleanup cannot remove: a directory with content
/// at the sidecar's path.
fn block_sidecar_removal(sidecar: PathBuf) -> impl FnOnce() {
    move || {
        std::fs::remove_file(&sidecar).expect("remove the staged sidecar");
        write_file(&sidecar.join("held"), b"held");
    }
}

#[test]
fn gateway_failure_runs_no_workshop_build_and_removes_a_stale_sidecar() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let triple = "x86_64-pc-windows-msvc";
    write_file(&test_environment.sidecar(triple), b"stale");
    let mut runner = FakeRunner::with_responses(vec![failure("gateway broke")]);

    let error = build_workshop(&debug_request(triple), environment, &mut runner)
        .expect_err("Gateway failure");

    assert!(error.primary.contains("Gateway build failed"), "{error}");
    assert!(error.primary.contains("gateway broke"), "{error}");
    assert_eq!(error.cleanup, None);
    assert_eq!(runner.commands.len(), 1);
    assert!(!test_environment.sidecar(triple).exists());
}

#[test]
fn workshop_failure_is_primary_when_cleanup_also_fails() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let triple = "x86_64-pc-windows-msvc";
    let mut runner = FakeRunner::with_responses(vec![
        gateway_built(debug_gateway(environment, triple)),
        act(
            block_sidecar_removal(test_environment.sidecar(triple)),
            failure("workshop broke"),
        ),
    ]);

    let error = build_workshop(&debug_request(triple), environment, &mut runner)
        .expect_err("Workshop and cleanup failure");

    assert!(error.primary.contains("Workshop build failed"), "{error}");
    assert!(error.primary.contains("workshop broke"), "{error}");
    let cleanup = error.cleanup.expect("separate cleanup failure");
    assert!(
        cleanup.contains("Gateway sidecar cleanup failed"),
        "{cleanup}"
    );
    assert!(cleanup.contains("cannot remove"), "{cleanup}");
}

#[test]
fn stage_failure_names_the_missing_source_skips_the_workshop_build_and_cleans_up() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let triple = "aarch64-pc-windows-msvc";
    write_file(&test_environment.sidecar(triple), b"stale");
    let mut runner = FakeRunner::with_responses(vec![success("")]);

    let error = build_workshop(&debug_request(triple), environment, &mut runner)
        .expect_err("staging failure");

    assert!(
        error.primary.contains("Gateway sidecar staging failed"),
        "{error}"
    );
    assert!(
        error
            .primary
            .contains(&debug_gateway(environment, triple).display().to_string()),
        "{error}"
    );
    assert_eq!(runner.commands.len(), 1);
    assert!(!test_environment.sidecar(triple).exists());
}

#[test]
fn interruption_during_the_gateway_build_stages_nothing() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let triple = "x86_64-pc-windows-msvc";
    let mut runner = FakeRunner::with_responses(vec![FakeResponse::InterruptedBeforeStart]);

    let error = build_workshop(&debug_request(triple), environment, &mut runner)
        .expect_err("Gateway interruption");

    assert!(
        error.primary.contains("Gateway build interrupted"),
        "{error}"
    );
    assert_eq!(error.cleanup, None);
    assert_eq!(runner.commands.len(), 1);
    assert!(!test_environment.sidecar(triple).exists());
}

#[test]
fn interruption_after_gateway_completion_stages_nothing() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let triple = "x86_64-pc-windows-msvc";
    let source = debug_gateway(environment, triple);
    let mut runner = FakeRunner::with_responses(vec![act(
        move || write_file(&source, b"gateway"),
        FakeResponse::CompletedAndInterrupted,
    )]);

    let error = build_workshop(&debug_request(triple), environment, &mut runner)
        .expect_err("pre-staging interruption");

    assert!(
        error
            .primary
            .contains("Gateway sidecar staging interrupted"),
        "{error}"
    );
    assert_eq!(runner.commands.len(), 1);
    assert!(!test_environment.sidecar(triple).exists());
}

#[test]
fn interruption_raced_with_completion_removes_the_sidecar() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let triple = "x86_64-pc-windows-msvc";
    let mut runner = FakeRunner::with_responses(vec![
        gateway_built(debug_gateway(environment, triple)),
        workshop_built_with_sidecar(
            test_environment.sidecar(triple),
            FakeResponse::CompletedAndInterrupted,
        ),
    ]);

    let error = build_workshop(&debug_request(triple), environment, &mut runner)
        .expect_err("Workshop completion race");

    assert!(
        error
            .primary
            .contains("Workshop build interrupted after child completion"),
        "{error}"
    );
    assert!(!test_environment.sidecar(triple).exists());
}

#[test]
fn interruption_raced_with_completion_fails_a_build_without_a_sidecar() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let triple = "x86_64-unknown-linux-gnu";
    let mut runner = FakeRunner::with_responses(vec![
        gateway_built(debug_gateway(environment, triple)),
        workshop_built_without_sidecar(
            test_environment.sidecar(triple),
            FakeResponse::CompletedAndInterrupted,
        ),
    ]);

    let error = build_workshop(&debug_request(triple), environment, &mut runner)
        .expect_err("Workshop completion race");

    assert!(
        error
            .primary
            .contains("Workshop build interrupted after child completion"),
        "{error}"
    );
    assert_eq!(error.cleanup, None);
}

#[test]
fn interruption_preserves_cleanup_failure_diagnostics() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let triple = "x86_64-pc-windows-msvc";
    let mut runner = FakeRunner::with_responses(vec![
        gateway_built(debug_gateway(environment, triple)),
        act(
            block_sidecar_removal(test_environment.sidecar(triple)),
            FakeResponse::InterruptedAfterStart,
        ),
    ]);

    let error = build_workshop(&debug_request(triple), environment, &mut runner)
        .expect_err("Workshop interruption and cleanup failure");

    assert!(
        error.primary.contains("Workshop build interrupted"),
        "{error}"
    );
    let cleanup = error.cleanup.expect("cleanup diagnostic");
    assert!(
        cleanup.contains("Gateway sidecar cleanup failed"),
        "{cleanup}"
    );
}

#[test]
fn cleanup_failure_after_success_fails_the_command_clearly() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let triple = "x86_64-pc-windows-msvc";
    let mut runner = FakeRunner::with_responses(vec![
        gateway_built(debug_gateway(environment, triple)),
        act(
            block_sidecar_removal(test_environment.sidecar(triple)),
            success(""),
        ),
    ]);

    let error = build_workshop(&debug_request(triple), environment, &mut runner)
        .expect_err("cleanup failure");

    assert!(
        error.primary.contains("Gateway sidecar cleanup failed"),
        "{error}"
    );
    assert_eq!(error.cleanup, None);
}

#[test]
fn malformed_host_output_fails_before_building() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let mut runner = FakeRunner::with_responses(vec![success("cargo 1.89.0\n")]);

    let error = build_workshop(
        &BuildRequest {
            profile: Profile::Debug,
            target: None,
        },
        environment,
        &mut runner,
    )
    .expect_err("missing host");

    assert!(error.primary.contains("Cargo host triple"), "{error}");
    assert_eq!(runner.commands.len(), 1);
}

#[test]
fn a_version_mismatch_stops_before_staging_and_removes_a_stale_sidecar() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let target = "x86_64-pc-windows-msvc";
    write_file(&test_environment.sidecar(target), b"stale");
    let mut runner = FakeRunner::with_responses(vec![
        node_found(),
        gateway_built(gateway(environment, target)),
        success("promptforge-gateway 0.0.1\n"),
    ]);

    let error = build_installer(&request(target, false), environment, &mut runner)
        .expect_err("version mismatch");

    assert!(
        error
            .primary
            .contains("printed `promptforge-gateway 0.0.1`"),
        "{error}"
    );
    assert!(
        error
            .primary
            .contains(&format!("expected `promptforge-gateway {VERSION}`")),
        "{error}"
    );
    assert_eq!(runner.commands.len(), 3);
    assert!(!test_environment.sidecar(target).exists());
}

#[test]
fn a_failed_version_check_stops_before_staging_and_removes_a_stale_sidecar() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let target = "aarch64-pc-windows-msvc";
    write_file(&test_environment.sidecar(target), b"stale");
    let mut runner = FakeRunner::with_responses(vec![
        node_found(),
        gateway_built(gateway(environment, target)),
        failure("bad CPU type in executable"),
    ]);

    let error = build_installer(&request(target, false), environment, &mut runner)
        .expect_err("version check failure");

    assert!(
        error
            .primary
            .contains("Gateway version check failed: bad CPU type in executable"),
        "{error}"
    );
    assert_eq!(runner.commands.len(), 3);
    assert!(!test_environment.sidecar(target).exists());
}

#[test]
fn a_failed_unstartable_or_interrupted_bundle_leaves_no_sidecar_and_collects_nothing() {
    let cases = || {
        [
            (
                failure("bundle broke"),
                "Workshop bundle failed: bundle broke",
            ),
            (
                FakeResponse::SpawnFailed(io::ErrorKind::NotFound, "node not found"),
                "Workshop bundle could not start: node not found",
            ),
            (
                FakeResponse::InterruptedAfterStart,
                "Workshop bundle interrupted",
            ),
        ]
    };
    for target in ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"] {
        for (response, expected) in cases() {
            let test_environment = environment();
            let environment = &test_environment.environment;
            let mut runner = FakeRunner::with_responses(vec![
                node_found(),
                gateway_built(gateway(environment, target)),
                version_printed(),
                bundled(
                    test_environment.sidecar(target),
                    target,
                    Vec::new(),
                    response,
                ),
            ]);

            let error = build_installer(&request(target, false), environment, &mut runner)
                .expect_err("bundle failure");

            assert!(error.primary.contains(expected), "{target}: {error}");
            assert!(!test_environment.sidecar(target).exists(), "{target}");
            assert!(!output(environment, target).exists(), "{target}");
        }
    }
}

#[test]
fn a_signer_that_writes_no_signature_fails_the_build() {
    let mut test_environment = environment();
    set_signing_key(&mut test_environment.environment);
    let environment = &test_environment.environment;
    let target = "x86_64-unknown-linux-gnu";
    let appimage = bundle(environment, target, "appimage")
        .join(format!("PromptForge_{VERSION}_amd64.AppImage"));
    let mut runner = FakeRunner::with_responses(vec![
        node_found(),
        gateway_built(gateway(environment, target)),
        version_printed(),
        bundled(
            test_environment.sidecar(target),
            target,
            vec![collect_signature(&appimage), appimage],
            success(""),
        ),
        success(""),
    ]);

    let error = build_installer(&request(target, true), environment, &mut runner)
        .expect_err("missing signature");

    assert!(error.primary.contains("wrote no signature"), "{error}");
}

#[test]
fn an_unsupported_target_fails_before_building() {
    let test_environment = environment();
    let mut runner = FakeRunner::default();

    let error = build_installer(
        &request("x86_64-unknown-freebsd", false),
        &test_environment.environment,
        &mut runner,
    )
    .expect_err("unsupported target");

    assert!(error.primary.contains("x86_64-unknown-freebsd"), "{error}");
    assert!(runner.commands.is_empty());
}

#[test]
fn a_missing_node_fails_before_the_gateway_build() {
    let test_environment = environment();
    let environment = &test_environment.environment;
    let target = "x86_64-unknown-linux-gnu";
    let mut runner = FakeRunner::with_responses(vec![FakeResponse::SpawnFailed(
        io::ErrorKind::NotFound,
        "program not found",
    )]);

    let error = build_installer(&request(target, false), environment, &mut runner)
        .expect_err("missing node");

    assert!(
        error
            .primary
            .contains("the Tauri CLI runs on `node` from PATH) could not start"),
        "{error}"
    );
    assert_eq!(runner.commands.len(), 1);
}
