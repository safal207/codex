use super::*;
use crate::app::test_support::make_test_app;
use crate::app::tests::make_test_app_with_channels;
use crate::chatwidget::tests::configure_app_event_test_thread;
use codex_send::SendRequest;
use codex_send::session_socket_path;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::time::Duration;

/// Unix socket paths are limited to about 104 bytes on macOS, and the default
/// temp directory there is already most of that.
fn short_codex_home() -> tempfile::TempDir {
    let builder = tempfile::Builder::new();
    #[cfg(unix)]
    let dir = builder.tempdir_in("/tmp");
    #[cfg(not(unix))]
    let dir = builder.tempdir();
    dir.expect("temp dir")
}

async fn wait_for_socket(path: &Path, expected: bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::fs::try_exists(path).await.expect("probe socket") != expected {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for socket {} to {}",
            path.display(),
            if expected { "appear" } else { "disappear" }
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn endpoint_moves_with_the_primary_thread_and_is_removed_on_reset() {
    let mut app = make_test_app().await;
    let codex_home = short_codex_home();
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.config.ipc_enabled = true;
    let first = ThreadId::new();
    let second = ThreadId::new();
    let first_socket = session_socket_path(codex_home.path(), first);
    let second_socket = session_socket_path(codex_home.path(), second);

    app.bind_send_endpoint(first);
    wait_for_socket(&first_socket, /*expected*/ true).await;

    app.bind_send_endpoint(second);
    wait_for_socket(&second_socket, /*expected*/ true).await;
    wait_for_socket(&first_socket, /*expected*/ false).await;

    // The test app has no event loop, so the reply channel closes unanswered.
    let response = codex_send::send_message(
        codex_home.path(),
        second,
        &SendRequest {
            id: "r1".to_string(),
            text: "hello".to_string(),
        },
    )
    .await
    .expect("round trip over the socket");
    assert_eq!(
        response,
        codex_send::SendResponse::Rejected {
            message: "the session did not accept the message".to_string(),
        }
    );

    app.reset_thread_event_state().await;
    assert!(app.send_endpoint.is_none());
    wait_for_socket(&second_socket, /*expected*/ false).await;
}

#[tokio::test]
async fn rebinding_the_same_thread_waits_for_the_previous_cleanup() {
    let mut app = make_test_app().await;
    let codex_home = short_codex_home();
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.config.ipc_enabled = true;
    let thread_id = ThreadId::new();
    let socket = session_socket_path(codex_home.path(), thread_id);

    app.bind_send_endpoint(thread_id);
    wait_for_socket(&socket, /*expected*/ true).await;
    app.reset_thread_event_state().await;
    app.bind_send_endpoint(thread_id);

    wait_for_socket(&socket, /*expected*/ true).await;
    // The aborted first task must not remove the socket the second task owns.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(tokio::fs::try_exists(&socket).await.expect("probe socket"));
}

#[tokio::test]
async fn endpoint_is_not_bound_when_ipc_is_disabled() {
    let mut app = make_test_app().await;
    let codex_home = short_codex_home();
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.config.ipc_enabled = false;

    app.bind_send_endpoint(ThreadId::new());

    assert!(app.send_endpoint.is_none());
    assert!(!codex_send::ipc_directory(codex_home.path()).exists());
}

// Dispatch tests use an inert endpoint identity instead of binding a socket. No model turn is
// dispatched: scoped operations are inspected, then rejected after a view/generation change.
fn install_test_endpoint(app: &mut App, thread_id: ThreadId) -> SendMessageScope {
    let generation = uuid::Uuid::new_v4();
    let scope = SendMessageScope {
        thread_id,
        endpoint_generation: generation,
    };
    app.config.ipc_enabled = true;
    app.primary_thread_id = Some(thread_id);
    app.active_thread_id = Some(thread_id);
    configure_app_event_test_thread(&mut app.chat_widget, thread_id);
    app.send_endpoint = Some(SendEndpointTask {
        thread_id,
        generation,
        socket_path: app
            .config
            .codex_home
            .join("unused-test-endpoint.sock")
            .to_path_buf(),
        bound: Arc::new(AtomicBool::new(true)),
        task: None,
    });
    scope
}

#[tokio::test]
async fn ipc_request_rejects_a_different_displayed_or_active_thread() -> color_eyre::Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let primary = ThreadId::new();
    let other = ThreadId::new();
    let scope = install_test_endpoint(&mut app, primary);
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    for (active, displayed) in [(other, other), (other, primary), (primary, other)] {
        app.active_thread_id = Some(active);
        configure_app_event_test_thread(&mut app.chat_widget, displayed);
        while events.try_recv().is_ok() {}
        let (reply, response) = oneshot::channel();
        app.handle_event(
            &mut tui,
            &mut server,
            AppEvent::SendMessageReceived {
                scope,
                request: SendRequest {
                    id: format!("{active}-{displayed}"),
                    text: "marker".to_string(),
                },
                reply,
            },
        )
        .await?;
        assert!(matches!(response.await?, SendResponse::Rejected { .. }));
        assert!(!std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
            matches!(
                event,
                AppEvent::CodexOp(_) | AppEvent::SubmitIpcThreadOp { .. }
            )
        }));
        assert!(ops.try_recv().is_err());
    }
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn ipc_request_rejects_an_old_generation_for_the_same_thread() -> color_eyre::Result<()> {
    let (mut app, _events, mut ops) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let previous = install_test_endpoint(&mut app, thread_id);
    let current = install_test_endpoint(&mut app, thread_id);
    assert_ne!(previous.endpoint_generation, current.endpoint_generation);
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let (reply, response) = oneshot::channel();
    app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::SendMessageReceived {
            scope: previous,
            request: SendRequest {
                id: "old-generation".to_string(),
                text: "marker".to_string(),
            },
            reply,
        },
    )
    .await?;
    assert!(matches!(response.await?, SendResponse::Rejected { .. }));
    assert!(ops.try_recv().is_err());
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn ipc_operation_revalidates_the_target_after_request_acceptance() -> color_eyre::Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let scope = install_test_endpoint(&mut app, thread_id);
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    while events.try_recv().is_ok() {}
    let (reply, response) = oneshot::channel();
    app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::SendMessageReceived {
            scope,
            request: SendRequest {
                id: "accepted-before-switch".to_string(),
                text: "marker".to_string(),
            },
            reply,
        },
    )
    .await?;
    assert_eq!(response.await?, SendResponse::Submitted);
    let operation = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::SubmitIpcThreadOp {
                scope: delivered_scope,
                op,
            } => {
                assert_eq!(delivered_scope, scope);
                Some(AppEvent::SubmitIpcThreadOp {
                    scope: delivered_scope,
                    op,
                })
            }
            AppEvent::CodexOp(_) => panic!("IPC must not produce an unscoped operation"),
            _ => None,
        })
        .expect("IPC submission should retain the endpoint identity");
    let other = ThreadId::new();
    app.active_thread_id = Some(other);
    configure_app_event_test_thread(&mut app.chat_widget, other);
    while events.try_recv().is_ok() {}
    // Neither ID is registered with the server. Reaching submit_thread_op would fail; the guard
    // must reject first, with no model call or operation for the newly selected thread.
    app.handle_event(&mut tui, &mut server, operation).await?;
    assert!(
        events.try_recv().is_err(),
        "a stale operation must not reach turn/start rejection or emit error/UI events on the displayed widget"
    );
    assert!(ops.try_recv().is_err());
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn ipc_deferred_operation_retains_scope_and_rejects_endpoint_replacement()
-> color_eyre::Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let scope = install_test_endpoint(&mut app, thread_id);
    app.chat_widget.set_queue_autosend_suppressed(true);
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    while events.try_recv().is_ok() {}
    let (reply, response) = oneshot::channel();
    app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::SendMessageReceived {
            scope,
            request: SendRequest {
                id: "queued-before-reset".to_string(),
                text: "!literal marker".to_string(),
            },
            reply,
        },
    )
    .await?;
    assert_eq!(response.await?, SendResponse::Queued);
    assert!(!std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
        matches!(
            event,
            AppEvent::CodexOp(_) | AppEvent::SubmitIpcThreadOp { .. }
        )
    }));
    app.chat_widget.set_queue_autosend_suppressed(false);
    app.chat_widget.maybe_send_next_queued_input();
    let operation = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::SubmitIpcThreadOp {
                scope: delivered_scope,
                op,
            } => {
                assert_eq!(delivered_scope, scope);
                Some(AppEvent::SubmitIpcThreadOp {
                    scope: delivered_scope,
                    op,
                })
            }
            AppEvent::CodexOp(_) => panic!("deferred IPC must not lose its scope"),
            _ => None,
        })
        .expect("draining IPC queue must produce a scoped operation");
    install_test_endpoint(&mut app, thread_id);
    while events.try_recv().is_ok() {}
    app.handle_event(&mut tui, &mut server, operation).await?;
    assert!(
        events.try_recv().is_err(),
        "an old generation must be rejected before any app/server pipeline mutation"
    );
    assert!(ops.try_recv().is_err());
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn ipc_scope_requires_an_enabled_owned_endpoint() {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let scope = install_test_endpoint(&mut app, ThreadId::new());
    assert!(app.send_message_scope_is_current(scope));
    app.config.ipc_enabled = false;
    assert!(!app.send_message_scope_is_current(scope));
    app.config.ipc_enabled = true;
    app.send_endpoint
        .as_ref()
        .expect("endpoint")
        .bound
        .store(false, Ordering::Release);
    assert!(!app.send_message_scope_is_current(scope));
    app.send_endpoint = None;
    assert!(!app.send_message_scope_is_current(scope));
}

#[tokio::test]
async fn ipc_optimistic_history_revalidates_scope_before_insertion() -> color_eyre::Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    for replace_endpoint in [false, true] {
        let scope = install_test_endpoint(&mut app, thread_id);
        while events.try_recv().is_ok() {}
        let (reply, response) = oneshot::channel();
        app.handle_event(
            &mut tui,
            &mut server,
            AppEvent::SendMessageReceived {
                scope,
                request: SendRequest {
                    id: format!("history-{replace_endpoint}"),
                    text: "IPC_HISTORY_MARKER".to_string(),
                },
                reply,
            },
        )
        .await?;
        assert_eq!(response.await?, SendResponse::Submitted);
        let history = std::iter::from_fn(|| events.try_recv().ok())
            .find_map(|event| match event {
                AppEvent::InsertIpcHistoryCell {
                    scope: delivered_scope,
                    cell,
                } => {
                    assert_eq!(delivered_scope, scope);
                    Some(AppEvent::InsertIpcHistoryCell {
                        scope: delivered_scope,
                        cell,
                    })
                }
                AppEvent::InsertHistoryCell(_) => {
                    panic!("IPC user prompt must not be globally inserted")
                }
                _ => None,
            })
            .expect("IPC prompt should have a scoped history insertion");
        if replace_endpoint {
            install_test_endpoint(&mut app, thread_id);
        } else {
            let other = ThreadId::new();
            app.active_thread_id = Some(other);
            configure_app_event_test_thread(&mut app.chat_widget, other);
        }
        let count_before = app.transcript_cells.len();
        app.handle_event(&mut tui, &mut server, history).await?;
        assert_eq!(app.transcript_cells.len(), count_before);
    }
    server.shutdown().await?;
    Ok(())
}
